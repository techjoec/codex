use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::state::TURN_OUTPUT_TRUNCATION_NOTICE;

const DEFAULT_MAX_LINES: usize = 160;
const DEFAULT_MAX_BYTES: usize = 8 * 1024;
const SMALL_FILE_MAX_LINES: usize = 400;
const SMALL_FILE_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadCodeArgs {
    path: String,
    #[serde(default)]
    lines: Option<LinesArg>,
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    max_bytes: Option<usize>,
    #[serde(default)]
    symbol: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LinesArg {
    Pair([usize; 2]),
    Object { start: usize, end: Option<usize> },
    Ranges(Vec<[usize; 2]>),
}

impl LinesArg {
    fn into_ranges(self) -> Result<Vec<(usize, usize)>, FunctionCallError> {
        match self {
            LinesArg::Pair([start, end]) => Ok(vec![(start, end)]),
            LinesArg::Object { start, end } => {
                let end = end.unwrap_or(start);
                Ok(vec![(start, end)])
            }
            LinesArg::Ranges(ranges) => {
                if ranges.is_empty() {
                    return Err(invalid_arguments("lines must include at least one range"));
                }
                if ranges.len() > 1 {
                    return Err(invalid_arguments(
                        "multiple line ranges are not supported yet; provide a single [start, end] range",
                    ));
                }
                let [start, end] = ranges[0];
                Ok(vec![(start, end)])
            }
        }
    }
}

pub(crate) async fn handle_read_code_tool_call(
    sess: &Session,
    turn_context: &TurnContext,
    arguments: String,
) -> Result<String, FunctionCallError> {
    let args: ReadCodeArgs = serde_json::from_str(&arguments)
        .map_err(|err| invalid_arguments(format!("failed to parse function arguments: {err}")))?;

    if args.path.trim().is_empty() {
        return Err(invalid_arguments("path must not be empty"));
    }

    if args.symbol.is_some() {
        return Err(invalid_arguments(
            "symbol lookups are not yet supported; request an explicit line range instead",
        ));
    }

    let resolved_path = turn_context.resolve_path(Some(args.path.clone()));
    validate_within_workspace(&resolved_path, &turn_context.cwd)?;

    let metadata = tokio::fs::metadata(&resolved_path).await.map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to read metadata for {path}: {err}",
            path = args.path
        ))
    })?;

    if !metadata.is_file() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{path} is not a regular file",
            path = args.path
        )));
    }

    let raw_contents = tokio::fs::read_to_string(&resolved_path)
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to read {path}: {err}",
                path = args.path
            ))
        })?;

    if raw_contents.is_empty() {
        let rel_path = display_path(&resolved_path, &turn_context.cwd);
        return Ok(format!("path: {rel_path}\n[notice] file is empty"));
    }

    let line_slices: Vec<&str> = raw_contents.split_inclusive('\n').collect();
    let line_count = line_slices.len();

    let mut requested_ranges = if let Some(lines) = args.lines {
        lines.into_ranges()?
    } else {
        vec![(1, line_count.max(1))]
    };

    normalize_ranges(&mut requested_ranges)?;

    let context_lines = args.context.unwrap_or_default() as usize;
    let contextualized = apply_context(&requested_ranges, context_lines, line_count);

    if contextualized.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "requested lines are outside the file".to_string(),
        ));
    }

    let requested_line_total = contextualized
        .iter()
        .map(|(start, end)| end.saturating_sub(*start).saturating_add(1))
        .sum::<usize>();

    let small_file_allowance =
        metadata.len() as usize <= SMALL_FILE_MAX_BYTES && line_count <= SMALL_FILE_MAX_LINES;

    let max_lines = if small_file_allowance {
        SMALL_FILE_MAX_LINES
    } else {
        DEFAULT_MAX_LINES
    };

    let requested_max_bytes = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let max_bytes_limit = if small_file_allowance {
        requested_max_bytes.min(SMALL_FILE_MAX_BYTES)
    } else {
        requested_max_bytes.min(DEFAULT_MAX_BYTES)
    };

    let rel_path = display_path(&resolved_path, &turn_context.cwd);

    let (uncovered_ranges, had_overlap) = sess
        .compute_unserved_code_ranges(&rel_path, &contextualized)
        .await;

    if uncovered_ranges.is_empty() {
        let mut output = format!("path: {rel_path}\n");
        output.push_str(
            "[notice] all requested lines were already provided earlier in this session; nothing new to show",
        );
        return Ok(output);
    }

    let uncovered_line_total = uncovered_ranges
        .iter()
        .map(|(start, end)| end.saturating_sub(*start).saturating_add(1))
        .sum::<usize>();

    let overlap_lines = requested_line_total.saturating_sub(uncovered_line_total);

    let (line_limited_ranges, truncated_by_lines) = enforce_line_cap(&uncovered_ranges, max_lines);

    if line_limited_ranges.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "requested slice exceeds the {max_lines}-line limit; narrow the range or request /relax"
        )));
    }

    let mut notices = Vec::new();
    if had_overlap && overlap_lines > 0 {
        notices.push(format!(
            "trimmed {overlap_lines} line(s) that were already served earlier in this session"
        ));
    }
    if truncated_by_lines {
        notices.push(format!(
            "truncated to {max_lines} line(s); request /relax for a temporary increase"
        ));
    }

    let (content, served_ranges, truncated_by_bytes) =
        build_content(&line_limited_ranges, &line_slices, max_bytes_limit);

    if served_ranges.is_empty() {
        let mut output = format!("path: {rel_path}\n");
        output.push_str(
            "[notice] byte budget exhausted before any new lines could be served; narrow the range or request /relax",
        );
        return Ok(output);
    }

    if truncated_by_bytes {
        notices.push(format!(
            "truncated to {max_bytes_limit} byte(s); request /relax for a temporary increase"
        ));
    }

    let mut header = format!("path: {rel_path}\n");
    for notice in &notices {
        header.push_str("[notice] ");
        header.push_str(notice);
        header.push('\n');
    }

    let mut output = header;
    if !content.is_empty() {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
        output.push_str(&content);
    }

    sess.record_served_code_ranges(&rel_path, &served_ranges)
        .await;

    let desired_bytes = output.as_bytes().len();
    let notice_len = TURN_OUTPUT_TRUNCATION_NOTICE.len();
    if let Some(decision) = sess
        .reserve_tool_output_budget(desired_bytes, notice_len)
        .await
    {
        if decision.truncated {
            truncate_string_to_bytes(&mut output, decision.allowed_content_bytes);
            let notice = truncated_notice(TURN_OUTPUT_TRUNCATION_NOTICE, decision.notice_bytes);
            if !notice.is_empty() {
                if !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&notice);
            }
        }
    }

    Ok(output)
}

fn normalize_ranges(ranges: &mut Vec<(usize, usize)>) -> Result<(), FunctionCallError> {
    for (start, end) in ranges.iter_mut() {
        if *start == 0 {
            return Err(invalid_arguments(
                "line numbers must be 1-indexed and greater than zero",
            ));
        }
        if *end == 0 {
            return Err(invalid_arguments(
                "line numbers must be 1-indexed and greater than zero",
            ));
        }
        if *end < *start {
            std::mem::swap(start, end);
        }
    }
    ranges.sort_by_key(|(start, _)| *start);
    Ok(())
}

fn apply_context(
    ranges: &[(usize, usize)],
    context: usize,
    line_count: usize,
) -> Vec<(usize, usize)> {
    let mut contextualized = Vec::with_capacity(ranges.len());
    for &(start, end) in ranges {
        if line_count == 0 {
            break;
        }
        let start = start.saturating_sub(context).max(1);
        let end = (end + context).min(line_count);
        if start <= end {
            contextualized.push((start, end));
        }
    }
    merge_ranges(&mut contextualized);
    contextualized
}

fn merge_ranges(ranges: &mut Vec<(usize, usize)>) {
    if ranges.is_empty() {
        return;
    }

    ranges.sort_by_key(|(start, _)| *start);
    let mut merged = Vec::with_capacity(ranges.len());
    let mut current = ranges[0];
    for &(start, end) in &ranges[1..] {
        if start <= current.1.saturating_add(1) {
            current.1 = current.1.max(end);
        } else {
            merged.push(current);
            current = (start, end);
        }
    }
    merged.push(current);
    *ranges = merged;
}

fn enforce_line_cap(ranges: &[(usize, usize)], max_lines: usize) -> (Vec<(usize, usize)>, bool) {
    let mut remaining = max_lines;
    let mut result = Vec::new();
    let mut truncated = false;

    for &(start, end) in ranges {
        if remaining == 0 {
            truncated = true;
            break;
        }
        let span = end.saturating_sub(start).saturating_add(1);
        let allowed = remaining.min(span);
        let actual_end = start + allowed - 1;
        result.push((start, actual_end));
        remaining = remaining.saturating_sub(allowed);
        if actual_end < end {
            truncated = true;
            break;
        }
    }

    (result, truncated)
}

fn build_content(
    ranges: &[(usize, usize)],
    lines: &[&str],
    max_bytes: usize,
) -> (String, Vec<(usize, usize)>, bool) {
    let mut content = String::new();
    let mut served = Vec::new();
    let mut used = 0usize;
    let mut first_segment = true;
    let mut truncated = false;

    for &(start, end) in ranges {
        let Some(first_line) = lines.get(start - 1) else {
            continue;
        };
        let label = format!("lines {start}-{end}:\n");
        let label_len = label.as_bytes().len();
        let first_line_len = first_line.as_bytes().len();

        let mut required = label_len + first_line_len;
        if !first_segment && !content.ends_with('\n') {
            required += 1;
        }

        if used + required > max_bytes {
            truncated = true;
            break;
        }

        if !first_segment && !content.ends_with('\n') {
            content.push('\n');
            used += 1;
        }

        content.push_str(&label);
        used += label_len;

        let mut actual_end = start - 1;
        for line_idx in start..=end {
            let Some(text) = lines.get(line_idx - 1) else {
                break;
            };
            let len = text.as_bytes().len();
            if used + len > max_bytes {
                truncated = true;
                break;
            }
            content.push_str(text);
            used += len;
            actual_end = line_idx;
        }

        if actual_end >= start {
            served.push((start, actual_end));
        }

        if actual_end < end {
            truncated = true;
            break;
        }

        first_segment = false;
    }

    (content, served, truncated)
}

fn validate_within_workspace(path: &Path, cwd: &Path) -> Result<(), FunctionCallError> {
    if path.starts_with(cwd) {
        return Ok(());
    }
    Err(FunctionCallError::RespondToModel(
        "paths outside the workspace are not allowed".to_string(),
    ))
}

fn display_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .map(PathBuf::from)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn invalid_arguments(msg: impl Into<String>) -> FunctionCallError {
    FunctionCallError::RespondToModel(msg.into())
}

fn truncate_string_to_bytes(text: &mut String, max_bytes: usize) {
    if max_bytes == 0 {
        text.clear();
        return;
    }
    if text.as_bytes().len() <= max_bytes {
        return;
    }
    let keep = take_bytes_at_char_boundary(text.as_str(), max_bytes).len();
    text.truncate(keep);
}

fn truncated_notice(template: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || template.is_empty() {
        return String::new();
    }
    take_bytes_at_char_boundary(template, max_bytes).to_string()
}

fn take_bytes_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut keep = 0;
    for (idx, ch) in text.char_indices() {
        let end = idx + ch.len_utf8();
        if end > max_bytes {
            break;
        }
        keep = end;
    }
    &text[..keep]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_ranges() {
        let mut ranges = vec![(5, 10), (1, 3), (3, 7), (20, 25)];
        merge_ranges(&mut ranges);
        assert_eq!(ranges, vec![(1, 10), (20, 25)]);
    }

    #[test]
    fn enforces_line_cap() {
        let ranges = vec![(1, 50), (60, 120)];
        let (limited, truncated) = enforce_line_cap(&ranges, 80);
        assert_eq!(limited, vec![(1, 50), (60, 89)]);
        assert!(truncated);
    }

    #[test]
    fn build_content_honors_byte_budget() {
        let lines = vec!["line1\n", "line2\n", "line3\n"];
        let ranges = vec![(1, 3)];
        let (content, served, truncated) = build_content(&ranges, &lines, 24);
        assert!(content.starts_with("lines 1-3:"));
        assert_eq!(served, vec![(1, 2)]);
        assert!(truncated);
    }
}

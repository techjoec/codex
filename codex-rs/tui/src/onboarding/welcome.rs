use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use crate::onboarding::onboarding_screen::StepStateProvider;
use crate::tui::FrameRequester;

use super::onboarding_screen::StepState;
use std::time::Duration;
use std::time::Instant;

// Embed animation frames for the Codex variant at compile time (same assets as new_model_popup.rs)
macro_rules! frames_for {
    ($dir:literal) => {
        [
            include_str!(concat!("../../frames/", $dir, "/frame_1.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_2.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_3.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_4.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_5.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_6.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_7.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_8.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_9.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_10.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_11.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_12.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_13.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_14.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_15.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_16.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_17.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_18.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_19.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_20.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_21.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_22.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_23.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_24.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_25.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_26.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_27.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_28.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_29.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_30.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_31.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_32.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_33.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_34.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_35.txt")),
            include_str!(concat!("../../frames/", $dir, "/frame_36.txt")),
        ]
    };
}

const FRAMES_DEFAULT: [&str; 36] = frames_for!("default");
const FRAME_TICK: Duration = Duration::from_millis(80);

pub(crate) struct WelcomeWidget {
    pub is_logged_in: bool,
    pub request_frame: FrameRequester,
    pub start: Instant,
}

impl WidgetRef for &WelcomeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        self.request_frame.schedule_frame_in(FRAME_TICK);

        let frames = &FRAMES_DEFAULT;
        let idx = if FRAME_TICK.as_millis() > 0 {
            let steps =
                (self.start.elapsed().as_millis() / FRAME_TICK.as_millis()) % frames.len() as u128;
            steps as usize
        } else {
            0
        };

        let mut lines: Vec<Line> = frames[idx].lines().map(|l| l.to_string().into()).collect();

        lines.push("".into());
        lines.push(Line::from(vec![
            "  ".into(),
            "Welcome to ".into(),
            "Codex".bold(),
            ", OpenAI's command-line coding agent".into(),
        ]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl StepStateProvider for WelcomeWidget {
    fn get_step_state(&self) -> StepState {
        match self.is_logged_in {
            true => StepState::Hidden,
            false => StepState::Complete,
        }
    }
}

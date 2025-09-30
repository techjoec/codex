Tokens are the building blocks of text that OpenAI models process. They can be as short as a single character or as long as a full word, depending on the language and context. Spaces, punctuation, and partial words all contribute to token counts. This is how the API internally segments your text before generating a response.

Helpful rules of thumb for English:

1 token ≈ 4 characters

1 token ≈ ¾ of a word

100 tokens ≈ 75 words

1–2 sentences ≈ 30 tokens

1 paragraph ≈ 100 tokens

~1,500 words ≈ 2,048 tokens

Tokenization can vary by language. For example, “Cómo estás” (Spanish for “How are you”) contains 5 tokens for 10 characters. Non-English text often produces a higher token-to-character ratio, which can affect costs and limits.

Examples
Here are some real-world text samples with their approximate token counts:

Wayne Gretzky’s quote “You miss 100% of the shots you don’t take” = 11 tokens

The OpenAI Charter = 476 tokens

The US Declaration of Independence = 1,695 tokens

How token counts are calculated
When you send text to the API:

The text is split into tokens.

The model processes these tokens.

The response is generated as a sequence of tokens, then converted back to text.

Token usage is tracked in several categories:

Input tokens – tokens in your request.

Output tokens – tokens generated in the response.

Cached tokens – reused tokens in conversation history (often billed at a reduced rate).

Reasoning tokens – in some advanced models, extra “thinking steps” are included internally before producing the final output.

These counts appear in your API response metadata and are used for billing and usage tracking.

To further explore tokenization, you can use our interactive Tokenizer tool, which allows you to calculate the number of tokens and see how text is broken into tokens.
​
Alternatively, if you'd like to tokenize text programmatically, use Tiktoken as a fast BPE tokenizer specifically used for OpenAI models.

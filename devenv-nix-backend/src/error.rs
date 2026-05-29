//! Helpers for shaping Nix evaluation errors into miette diagnostics.
//!
//! These work on the Nix C bindings' already-rendered error text. There is no
//! structured error type exposed by `nix-bindings-rust` to key off; the
//! bindings stringify the C++ exception's `what()` into a single message and
//! return it as `anyhow::Error`. So everything here is a textual heuristic
//! against Nix's renderer output — robust enough for current Nix versions,
//! and self-degrading: if the format changes such that no `error:` line is
//! found, callers fall back to rendering the message as one block.

/// Skip leading ANSI SGR escape sequences (`ESC [ … m`) so callers can match
/// on the textual prefix of a colored log line. Nix's logger emits messages
/// like `\x1b[31;1merror:\x1b[0m …`, and the leading color codes would
/// otherwise hide the `error:` keyword from a `starts_with` check.
pub(crate) fn strip_leading_ansi(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    while bytes.get(i) == Some(&0x1b) && bytes.get(i + 1) == Some(&b'[') {
        match bytes[i + 2..].iter().position(|&b| b == b'm') {
            Some(end) => i += 2 + end + 1,
            None => break,
        }
    }
    &s[i..]
}

/// Split a Nix evaluation diagnostic into `(trace, error_block)`.
///
/// With `--show-trace` enabled, Nix prints a tree of `… while …` frames
/// terminated by a final `error: …` paragraph. That paragraph carries the
/// actionable message (the assertion text, the syntax error, etc.); the frames
/// above it are useful for debugging but bury the message under ~100 lines of
/// internal evaluation context. Splitting lets the caller surface the
/// paragraph as the headline diagnostic and demote the trace.
///
/// Match rule: an `error:` line counts as a paragraph boundary only when it
/// is preceded by a blank line (or sits at the start of input). Once there is
/// leading trace/noise before the first matching `error:` line, that first
/// match is the Nix boundary; everything after it belongs to the user-facing
/// error block, even if a user-authored multi-line `throw` contains later
/// blank-line paragraphs that start with `error:`.
///
/// Returns `(None, text)` when no qualifying `error:` line is found, so callers
/// can fall back to a single-block rendering.
pub(crate) fn split_trailing_error(text: &str) -> (Option<&str>, &str) {
    let mut error_start: Option<usize> = None;
    let mut prev_was_blank = true; // start of input counts as "preceded by blank"
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let line_no_eol = line.strip_suffix('\n').unwrap_or(line);
        let is_blank = line_no_eol.trim().is_empty();
        if !is_blank && prev_was_blank {
            let after_indent = line_no_eol.trim_start();
            let after_ansi = strip_leading_ansi(after_indent);
            if after_ansi.starts_with("error:") {
                match error_start {
                    // Leading trace precedes the first `error:` paragraph, so
                    // that first match is the Nix boundary; everything after it
                    // belongs to the user-facing error block.
                    None if line_start > 0 => return split_at(text, line_start),
                    // Input opens with `error:` (no leading trace): treat the
                    // paragraphs as chained errors and let the last one win.
                    _ => error_start = Some(line_start),
                }
            }
        }
        prev_was_blank = is_blank;
        line_start += line.len();
    }
    match error_start {
        Some(start) => split_at(text, start),
        None => (None, text),
    }
}

/// Split `text` at byte offset `start` into `(trace, error_block)`, trimming
/// trailing whitespace from each side. A trace that trims down to empty (e.g.
/// the input opened with `error:` or only blank lines preceded it) is reported
/// as `None` rather than `Some("")`, so callers don't render an empty trace.
fn split_at(text: &str, start: usize) -> (Option<&str>, &str) {
    let trace = text[..start].trim_end();
    let trace = (!trace.is_empty()).then_some(trace);
    (trace, text[start..].trim_end())
}

/// Shape a raw Nix error string into a `MietteDiagnostic` for rendering.
///
/// Pure function over the FFI return text so it can be unit-tested without
/// running an actual evaluation. The caller wraps the result with
/// `Report::from(..)` to get a `miette::Error`.
///
/// Behavior:
/// - Dedents Nix's `--show-trace` indentation.
/// - If a trailing `error: …` paragraph is present, uses it as the headline
///   and demotes the preceding frames into the `help` field.
/// - Otherwise renders the whole input as the headline (graceful fallback
///   for outputs that don't match the assumed shape — e.g. a stray warning
///   surfaced through the FFI, or a future Nix format change).
pub(crate) fn format_eval_error(raw: &str, context: &str) -> miette::MietteDiagnostic {
    let dedented = dedent_lines(raw);
    let (trace, tail) = split_trailing_error(&dedented);
    match trace {
        Some(trace) => miette::diagnostic!(
            help = format!("Nix evaluation trace:\n\n{trace}"),
            "{context}: {tail}"
        ),
        None => miette::diagnostic!("{context}: {tail}"),
    }
}

/// Strip the longest leading whitespace common to non-blank, non-zero-indent
/// lines. Nix's `--show-trace` indents each trace line; removing the common
/// prefix flattens the rendered diagnostic without altering relative depth
/// (so source-pointer carets stay aligned).
pub(crate) fn dedent_lines(text: &str) -> String {
    let min_indent = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.bytes().take_while(|b| *b == b' ').count())
        .filter(|n| *n > 0)
        .min()
        .unwrap_or(0);
    if min_indent == 0 {
        return text.to_string();
    }
    text.lines()
        .map(|l| {
            let strip = l.bytes().take_while(|b| *b == b' ').count().min(min_indent);
            &l[strip..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_trailing_error_last_wins_for_four_chained_errors() {
        // Regression: with no leading trace and 4+ chained `error:` paragraphs,
        // the last paragraph must win (only it becomes the headline). The buggy
        // early-return used to split at the third error, leaking two paragraphs
        // into the tail.
        let text = "error: A\n\nerror: B\n\nerror: C\n\nerror: D";
        let (trace, tail) = split_trailing_error(text);
        assert_eq!(trace, Some("error: A\n\nerror: B\n\nerror: C"));
        assert_eq!(tail, "error: D");
    }

    #[test]
    fn strip_leading_ansi_handles_no_codes() {
        assert_eq!(strip_leading_ansi("error: plain"), "error: plain");
        assert_eq!(strip_leading_ansi(""), "");
    }

    #[test]
    fn strip_leading_ansi_skips_sgr_sequences() {
        assert_eq!(
            strip_leading_ansi("\u{1b}[31;1merror:\u{1b}[0m foo"),
            "error:\u{1b}[0m foo"
        );
        assert_eq!(strip_leading_ansi("\u{1b}[31m\u{1b}[1merror:"), "error:");
    }

    #[test]
    fn split_trailing_error_returns_text_when_no_error_line() {
        let (trace, tail) = split_trailing_error("just some prose\nwith no error keyword");
        assert_eq!(trace, None);
        assert_eq!(tail, "just some prose\nwith no error keyword");
    }

    #[test]
    fn split_trailing_error_extracts_final_error_block() {
        // Reproduces the missing-input case from issue #2820 follow-ups:
        // Nix emits ~100 lines of `--show-trace` frames followed by the
        // actionable `error: Failed assertions:` block. Surface the block.
        let text = "\
… from call site
  at /tmp/devenv.nix:1:1:
… while calling 'throw' builtin
  at /nix/store/.../top-level.nix:45:7:

error: Failed assertions:
- To use 'git-hooks', run the following command:

    $ devenv inputs add git-hooks github:cachix/git-hooks.nix --follows nixpkgs";
        let (trace, tail) = split_trailing_error(text);
        let trace = trace.expect("leading frames should be returned as the trace");
        assert!(trace.starts_with("… from call site"));
        assert!(trace.ends_with("top-level.nix:45:7:"));
        assert!(tail.starts_with("error: Failed assertions:"));
        assert!(tail.contains("devenv inputs add git-hooks"));
    }

    #[test]
    fn split_trailing_error_picks_last_error_when_multiple_present() {
        // Two error paragraphs separated by a blank line (how Nix renders
        // chained errors via `addErrorContext` and friends). The newer one
        // at the bottom wins; the older one stays in the trace.
        let text = "error: first error\n  some context\n\nerror: real error\n  details";
        let (trace, tail) = split_trailing_error(text);
        assert_eq!(trace, Some("error: first error\n  some context"));
        assert_eq!(tail, "error: real error\n  details");
    }

    #[test]
    fn split_trailing_error_ignores_error_keyword_inside_user_throw_body() {
        // Regression for the multi-line user-throw bug: a `throw ''…''` whose
        // body happens to contain `error: ` on one of its lines (without a
        // preceding blank line) must not be picked as the split point. The
        // real Nix `error:` keyword that opens the paragraph is what wins.
        let text = "\
… while calling the 'throw' builtin
  at /tmp/devenv.nix:3:15:

error: Step 1: do this
error: but this is the actual problem
Step 3: then this";
        let (trace, tail) = split_trailing_error(text);
        let trace = trace.expect("the throw frames should be returned as the trace");
        assert!(trace.contains("… while calling the 'throw' builtin"));
        assert!(
            tail.starts_with("error: Step 1: do this"),
            "headline should open with Nix's real error keyword (the first \
             error line), not pick the user's literal `error:` text from \
             the middle of the throw body. tail was: {tail}"
        );
        assert!(
            tail.contains("but this is the actual problem"),
            "the rest of the user's throw body should stay in the headline"
        );
        assert!(
            tail.ends_with("Step 3: then this"),
            "the trailing user line should stay in the headline"
        );
    }

    #[test]
    fn split_trailing_error_ignores_error_paragraph_inside_user_throw_body() {
        // Same as above, but with a blank line before the user's literal
        // `error:` line. Once the Nix boundary is found, later paragraphs are
        // still part of the user-facing error body.
        let text = "\
… while calling the 'throw' builtin
  at /tmp/devenv.nix:3:15:

error: Step 1: do this

error: but this is the actual problem
Step 3: then this";
        let (trace, tail) = split_trailing_error(text);
        let trace = trace.expect("the throw frames should be returned as the trace");
        assert!(trace.contains("… while calling the 'throw' builtin"));
        assert!(
            tail.starts_with("error: Step 1: do this"),
            "headline should open with Nix's real error keyword. tail was: {tail}"
        );
        assert!(
            tail.contains("\n\nerror: but this is the actual problem"),
            "the blank-line paragraph inside the throw body should stay in the headline"
        );
        assert!(tail.ends_with("Step 3: then this"));
    }

    #[test]
    fn split_trailing_error_handles_ansi_prefixed_error_line() {
        // Real Nix output has a blank line between the trace and the
        // ANSI-colored `error:` paragraph.
        let text = "trace context\n\n\u{1b}[31;1merror:\u{1b}[0m boom";
        let (trace, tail) = split_trailing_error(text);
        assert_eq!(trace, Some("trace context"));
        assert!(tail.contains("boom"));
    }

    #[test]
    fn split_trailing_error_handles_syntax_error_with_no_trace() {
        // Syntax errors arrive as a single `error: …` paragraph with source
        // context — no preceding frames. We should return an empty trace so
        // the caller renders the error inline without a help section.
        let text = "error: syntax error, unexpected '}', expecting ';'\n       at /path/to/devenv.nix:5:1:";
        let (trace, tail) = split_trailing_error(text);
        assert_eq!(trace, None);
        assert!(tail.starts_with("error: syntax error"));
        assert!(tail.contains("devenv.nix"));
    }

    #[test]
    fn split_trailing_error_reports_blank_only_lead_as_no_trace() {
        // The `error:` paragraph is preceded only by blank lines: there is no
        // real trace, so the (whitespace-only) lead must trim to `None` rather
        // than leaking a `Some("")` that callers would render as an empty help.
        let (trace, tail) = split_trailing_error("\n\nerror: boom");
        assert_eq!(trace, None);
        assert_eq!(tail, "error: boom");
    }

    // ---------------------------------------------------------------------
    // `format_eval_error` — end-to-end shaping of the diagnostic message.
    // Covers the two original symptoms of issue #2820 (warning shadowing the
    // real error, and the actionable bit being buried under the trace) plus
    // the degenerate / graceful-fallback shapes.

    #[test]
    fn format_eval_error_surfaces_trailing_error_and_demotes_trace() {
        // Missing-input shape: Nix `--show-trace` frames followed by an
        // assertion paragraph. The headline must be the assertion; the frames
        // must end up in `help` so the user isn't scrolling past 100 lines.
        let raw = "\
… from call site
  at /tmp/devenv.nix:1:1:
… while calling 'throw' builtin

error: Failed assertions:
- To use 'git-hooks', run the following command:

    $ devenv inputs add git-hooks github:cachix/git-hooks.nix --follows nixpkgs";
        let diag = format_eval_error(raw, "Failed to get shell attribute from devenv");
        assert!(
            diag.message
                .starts_with("Failed to get shell attribute from devenv: "),
            "headline should be prefixed with the context, got: {}",
            diag.message
        );
        assert!(
            diag.message.contains("devenv inputs add git-hooks"),
            "headline should carry the actionable suggestion, got: {}",
            diag.message
        );
        let help = diag.help.expect("trace should be demoted to help");
        assert!(help.starts_with("Nix evaluation trace:"));
        assert!(help.contains("… from call site"));
        assert!(
            !diag.message.contains("… from call site"),
            "frames should be in `help`, not the headline"
        );
    }

    #[test]
    fn format_eval_error_handles_bare_syntax_error_without_help() {
        // Syntax-error shape from the original #2820 reproduction. The whole
        // message is a single `error: …` paragraph with source context — no
        // frames, no `help:` section.
        let raw =
            "error: syntax error, unexpected '}', expecting ';'\n       at /tmp/devenv.nix:5:1:";
        let diag = format_eval_error(raw, "Failed to get attribute 'config.cachix.enable'");
        assert!(diag.message.contains("syntax error"));
        assert!(diag.message.contains("devenv.nix"));
        assert!(
            diag.help.is_none(),
            "no preceding trace, so no help expected (got: {:?})",
            diag.help
        );
    }

    #[test]
    fn format_eval_error_does_not_let_a_preceding_warning_shadow_the_error() {
        // Regression lock for the original #2820 symptom: a stale Nix
        // `warning: …` line arriving in the same diagnostic must not become
        // the headline. The trailing `error: …` line wins, the warning ends
        // up in the trace (the `help` field).
        let raw = "\
warning: Ignoring the client-specified setting 'system', because it is a restricted setting and you are not a trusted user

error: syntax error, unexpected '}', expecting ';'
       at /tmp/devenv.nix:5:1:";
        let diag = format_eval_error(raw, "Failed to get shell attribute from devenv");
        assert!(
            diag.message.contains("syntax error"),
            "headline should be the syntax error, got: {}",
            diag.message
        );
        assert!(
            !diag
                .message
                .contains("Ignoring the client-specified setting"),
            "the warning must not be the headline, got: {}",
            diag.message
        );
        let help = diag
            .help
            .expect("the preceding warning should land in help");
        assert!(help.contains("Ignoring the client-specified setting"));
    }

    #[test]
    fn format_eval_error_gracefully_renders_input_with_no_error_line() {
        // Degraded shape — if Nix ever returns text that doesn't contain an
        // `error:` line (format change, unrelated diagnostic, etc.), we still
        // want to show the user *something* coherent rather than crash or
        // produce an empty headline. The whole text becomes the headline.
        let raw = "some unexpected text\nthat does not contain the keyword";
        let diag = format_eval_error(raw, "Failed to evaluate");
        assert!(diag.message.contains("some unexpected text"));
        assert!(diag.message.contains("does not contain the keyword"));
        assert!(diag.help.is_none());
    }

    #[test]
    fn format_eval_error_strips_common_indentation_from_help() {
        // Nix's C-bindings logger wraps trace output in a uniform left margin
        // (7 spaces in practice). `dedent_lines` strips that so help is read
        // at the natural depth instead of being shoved off-screen.
        let raw = "\
       … from call site
         at /tmp/devenv.nix:1:1:

       error: boom";
        let diag = format_eval_error(raw, "ctx");
        let help = diag.help.expect("expected demoted trace in help");
        // After dedent the frame line begins at column 0, not column 7.
        assert!(
            help.lines().any(|l| l.starts_with("… from call site")),
            "frame line should be dedented to start at column 0, help was:\n{help}"
        );
    }
}

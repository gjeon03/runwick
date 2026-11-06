//! Recognising prompts that need the person, from the text of the current screen.
//!
//! Only prompts verified against the installed CLIs are recognised (strings read from Claude
//! Code 2.1.319 and codex-cli 0.164.0 and their screens). Anything else is not guessed: the
//! CLI's own screen stays the place to answer.
//!
//! - Claude Code permission dialogs: a question such as "Do you want to proceed?" /
//!   "Do you want to make this edit to …?" with numbered options ("1. Yes", "No, and tell
//!   Claude what to do differently").
//! - Codex approvals: "Would you like to run the following command?" (and edits/permissions)
//!   with "Yes, proceed" / "No, and tell Codex what to do differently".
//! - Trust and login (Auth): Claude's "Quick safety check: Is this a project you created or
//!   one you trust?" / "Yes, I trust this folder", "Select login method:"; Codex's
//!   "Trust this folder?" and "Sign in with ChatGPT".
//! - Claude's plan approval ("Would you like to proceed?"), "New MCP server found in this
//!   project" / "N new MCP servers found in this project" and plugin installs ("Would you like
//!   to install it?").
//!
//! - Any other numbered menu at the bottom of the screen (codex-cli's "Update available" with
//!   "› 1. Update now / 2. Skip"): a choice the person makes there ([`AttentionKind::Choice`]).
//!
//! [`ready_for_input`] says the opposite: the CLI's own input prompt is on screen, so an
//! instruction may be typed (collaboration delivery holds on anything else).

use runwick_core::api::sessions::{AttentionKind, Tool};

/// Only the bottom of what is drawn is considered: prompts render there while they are active,
/// and older text above may quote the same words.
const LOOK_BACK: usize = 30;

/// The prompt that needs the person on this screen, if any. `cursor_row` is where the program
/// writes now.
pub fn detect(tool: Tool, lines: &[String], cursor_row: usize) -> Option<(AttentionKind, String)> {
    if tool == Tool::Shell {
        return None;
    }
    let window = &lines[window(lines, cursor_row)];
    let tail: Vec<&str> = window.iter().map(|l| l.trim()).collect();
    let options_visible = |from: usize| tail[from..].iter().any(|l| is_yes_option(l));

    for (i, line) in tail.iter().enumerate() {
        let text = strip_frame(line);
        if is_trust_prompt(text) || is_login_prompt(text) {
            return Some((AttentionKind::Auth, text.to_string()));
        }
        if is_standalone_question(text) {
            return Some((AttentionKind::Approval, text.to_string()));
        }
        if is_approval_question(text) && options_visible(i + 1) {
            return Some((AttentionKind::Approval, text.to_string()));
        }
    }
    // A numbered menu none of the above covers (codex-cli's "Update available" choice, …).
    choice_menu(window).map(|text| (AttentionKind::Choice, text))
}

/// How often the text shows the CLI's "nothing saved to resume" answer (Claude Code's "No
/// conversation found with session ID", Codex's missing rollout/session), from the strings in
/// the installed CLIs.
pub fn resume_failures(tool: Tool, text: &str) -> usize {
    let phrases: &[&str] = match tool {
        Tool::Claude => &["No conversation found with session ID"],
        Tool::Codex => &["no rollout found for", "No active or archived session found"],
        Tool::Shell => &[],
    };
    phrases.iter().map(|p| text.matches(p).count()).sum()
}

/// The rows to scan: the last `LOOK_BACK` rows down to the last drawn row or the cursor,
/// whichever is lower. Blank rows below both are not part of the window, so a prompt drawn near
/// the top of a mostly empty screen (a fresh or tall terminal) is still within it.
fn window(lines: &[String], cursor_row: usize) -> std::ops::Range<usize> {
    let drawn = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
    let end = drawn.max(cursor_row + 1).min(lines.len());
    end.saturating_sub(LOOK_BACK)..end
}

/// Drop box-drawing borders and selection markers around a line ("│ ❯ 1. Yes │").
fn strip_frame(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '┃' | '║' | '|' | '❯' | '›' | '>' | '▌'))
}

fn is_approval_question(t: &str) -> bool {
    const QUESTIONS: &[&str] = &[
        // Claude Code
        "Do you want to proceed?",
        "Do you want to make this edit",
        "Do you want to create ",
        "Do you want to allow ",
        "Do you want to continue?",
        "Do you want to use this API key?",
        // Plan approval ("Yes, auto-accept edits", "Yes, manually approve edits", "No, keep planning").
        "Would you like to proceed?",
        // Codex
        "Would you like to run the following command?",
        "Would you like to make the following edits?",
        "Would you like to grant these permissions?",
        "Would you like to send input to the existing terminal?",
    ];
    QUESTIONS.iter().any(|q| t.contains(q))
}

/// Claude Code questions whose options do not start with "Yes" (strings of 2.1.319).
fn is_standalone_question(t: &str) -> bool {
    t.starts_with("New MCP server found in this project")
        || t.contains(" new MCP servers found in this project")
        || t.contains("Would you like to install it?")
        || t.contains("Would you like to install this LSP plugin?")
}

/// The CLI shows its own input prompt and nothing on screen asks for a choice: an instruction
/// can be typed there. Only shapes seen on the installed CLIs count (anything else holds):
/// Claude Code 2.1.319 draws `❯` + space between two `─` rules; codex-cli 0.164.0 draws its
/// composer `› …` among the last lines. A numbered choice under a marker (`❯ 1.`, `› 1.`,
/// `> 1.`) or a y/n question anywhere near the cursor means a dialog, never the input.
pub fn ready_for_input(tool: Tool, lines: &[String], cursor_row: usize) -> bool {
    let w = &lines[window(lines, cursor_row)];
    if w.iter().any(|l| is_choice_line(l)) {
        return false;
    }
    match tool {
        Tool::Claude => claude_input_box(w),
        Tool::Codex => codex_composer(w),
        Tool::Shell => false,
    }
}

/// `❯ 1. …`, `› 2. …`, `> 1. …` (a selected or listed choice) or a y/n question.
fn is_choice_line(line: &str) -> bool {
    let t = line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '┃' | '║' | '|'));
    let marked = ['❯', '›', '>'].iter().any(|m| {
        t.strip_prefix(*m).is_some_and(|rest| {
            let rest = rest.trim_start_matches(|c: char| c.is_whitespace());
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            digits > 0 && rest[digits..].starts_with('.')
        })
    });
    let lower = t.to_ascii_lowercase();
    marked || lower.contains("[y/n]") || lower.contains("(y/n)")
}

/// A numbered menu at the bottom of the screen: an option under the selection marker (`› 1.`,
/// `❯ 2.`) with another numbered option next to it, nothing in progress ("esc to interrupt"),
/// and below it no input prompt of the CLI and no more than its key hints. The text is the
/// first line of the paragraph just above the options (else the selected option).
fn choice_menu(w: &[String]) -> Option<String> {
    if w.iter().any(|l| l.to_ascii_lowercase().contains("esc to interrupt")) {
        return None;
    }
    let selected = w.iter().rposition(|l| option_number(l).is_some_and(|(marked, _)| marked))?;
    // Text typed into Claude Code's input box (`❯` right under its top rule) is not a menu.
    if selected > 0 && is_rule(&w[selected - 1]) {
        return None;
    }
    // The options: numbered lines around the selected one, at most one other line apart.
    let mut first = selected;
    let mut last = selected;
    let mut numbers = vec![option_number(&w[selected])?.1];
    let mut i = selected;
    while i > first.saturating_sub(2) {
        i -= 1;
        if let Some((_, n)) = option_number(&w[i]) {
            numbers.push(n);
            first = i;
        }
    }
    let mut i = selected + 1;
    while i < w.len() && i <= last + 2 {
        if let Some((_, n)) = option_number(&w[i]) {
            numbers.push(n);
            last = i;
        }
        i += 1;
    }
    numbers.sort_unstable();
    numbers.dedup();
    if numbers.len() < 2 {
        return None;
    }
    let below = &w[last + 1..];
    let prompt_below = below.iter().any(|l| input_after(l, '›') || input_after(l, '❯'));
    if prompt_below || below.iter().filter(|l| !l.trim().is_empty()).count() > BELOW_INPUT {
        return None;
    }
    let paragraph = w[..first]
        .iter()
        .rev()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty() && !is_rule(l))
        .last();
    let text = paragraph.map_or(&w[selected], |l| l);
    Some(strip_frame(text.trim()).to_string())
}

/// `› 1. …` / `2. …`: whether the option is under the selection marker, and its number.
fn option_number(line: &str) -> Option<(bool, &str)> {
    let t = line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '┃' | '║' | '|'));
    let (marked, rest) = match t.strip_prefix(['›', '❯']) {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let after = rest[digits..].strip_prefix('.')?;
    (digits > 0 && after.starts_with(char::is_whitespace)).then(|| (marked, &rest[..digits]))
}

/// After the marker: a space (or no-break space), or nothing.
fn input_after(line: &str, marker: char) -> bool {
    let t = line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '┃'));
    t.strip_prefix(marker).is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\u{a0}']))
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 8 && t.chars().all(|c| c == '─')
}

/// Lines drawn below the input box at most: its bottom rule, the status line and hints.
const BELOW_INPUT: usize = 6;

fn claude_input_box(w: &[String]) -> bool {
    let Some(i) = w.iter().rposition(|l| input_after(l, '❯')) else {
        return false;
    };
    let above = w[..i].iter().rev().take(2).any(|l| is_rule(l));
    let below = w[i + 1..].iter().take(15).any(|l| is_rule(l));
    // At the bottom: an old box with a program's output under it is not the input.
    let drawn_after = w[i + 1..].iter().filter(|l| !l.trim().is_empty()).count();
    above && below && drawn_after <= BELOW_INPUT
}

fn codex_composer(w: &[String]) -> bool {
    let drawn: Vec<&String> = w.iter().filter(|l| !l.trim().is_empty()).collect();
    drawn.iter().rev().take(6).any(|l| input_after(l, '›'))
}

fn is_yes_option(line: &str) -> bool {
    let t = strip_frame(line);
    // "1. Yes", "1. Yes, proceed (y)", "2. Yes, and don't ask again …"
    let after_number = t.trim_start_matches(|c: char| c.is_ascii_digit()).strip_prefix('.').map(str::trim_start);
    after_number.is_some_and(|rest| rest.starts_with("Yes")) || t.starts_with("Yes, proceed")
}

fn is_trust_prompt(t: &str) -> bool {
    t.contains("Is this a project you created or one you trust?")
        || t.contains("Yes, I trust this folder")
        || t.starts_with("Trust this folder?")
        || t.contains("Do you trust the files in this folder?")
}

fn is_login_prompt(t: &str) -> bool {
    t.starts_with("Select login method:") || t.starts_with("Sign in with ChatGPT")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// Detection with the cursor on the last line of `lines`.
    fn detect_at_end(tool: Tool, lines: &[String]) -> Option<(AttentionKind, String)> {
        detect(tool, lines, lines.len().saturating_sub(1))
    }

    const CODEX_APPROVAL: &str = "Would you like to run the following command?\n\
        \n  $ pnpm test:e2e --grep login\n\n\
        › 1. Yes, proceed (y)\n\
          2. No, and tell Codex what to do differently (esc)";

    #[test]
    fn a_prompt_near_the_top_of_a_mostly_empty_screen_is_found() {
        // A 60-row terminal: the prompt is drawn at the top, the cursor sits on its options and
        // everything below is blank. The last 30 rows alone would be empty.
        let mut s = screen(CODEX_APPROVAL);
        let cursor = s.len() - 2;
        s.resize(60, String::new());
        let found = detect(Tool::Codex, &s, cursor);
        assert_eq!(found.map(|a| a.0), Some(AttentionKind::Approval));
    }

    #[test]
    fn the_window_reaches_down_to_the_cursor() {
        // The program moved on: the old prompt is far above where it writes now.
        let mut s = screen(CODEX_APPROVAL);
        s.resize(60, String::new());
        assert_eq!(detect(Tool::Codex, &s, 50), None);
        assert_eq!(window(&s, 50), 21..51);
        // Trailing blank rows below both the text and the cursor are not counted.
        assert_eq!(window(&s, 2), 0..6);
        assert_eq!(window(&[], 0), 0..0);
    }

    #[test]
    fn claude_permission_dialog_is_an_approval() {
        let s = screen(
            "● I'll run the tests.\n\
             ╭──────────────────────────────────────╮\n\
             │ Bash command                          │\n\
             │   pnpm test auth                      │\n\
             │ Do you want to proceed?               │\n\
             │ ❯ 1. Yes                              │\n\
             │   2. Yes, and don't ask again for pnpm│\n\
             │   3. No, and tell Claude what to do differently (esc) │\n\
             ╰──────────────────────────────────────╯",
        );
        assert_eq!(detect_at_end(Tool::Claude, &s), Some((AttentionKind::Approval, "Do you want to proceed?".into())));
    }

    #[test]
    fn codex_command_approval_is_an_approval() {
        let s = screen(
            "Would you like to run the following command?\n\
             \n  $ pnpm test:e2e --grep login\n\n\
             › 1. Yes, proceed (y)\n\
               2. Yes, and don't ask again for this command (a)\n\
               3. No, and tell Codex what to do differently (esc)",
        );
        let (kind, text) = detect_at_end(Tool::Codex, &s).unwrap();
        assert_eq!(kind, AttentionKind::Approval);
        assert_eq!(text, "Would you like to run the following command?");
    }

    #[test]
    fn quoted_question_without_options_is_not_an_approval() {
        let s = screen("● The script printed \"Do you want to proceed?\" and exited.\n> ");
        assert_eq!(detect_at_end(Tool::Claude, &s), None);
    }

    #[test]
    fn trust_and_login_screens_are_auth() {
        let s = screen(
            "Accessing workspace:\n/tmp/x\nQuick safety check: Is this a project you created or one you trust?\n❯ 1. Yes, I trust this folder\n  2. No, exit",
        );
        assert_eq!(detect_at_end(Tool::Claude, &s).map(|a| a.0), Some(AttentionKind::Auth));
        let s = screen("> Trust this folder? Codex can read, edit, and run files here.\n  1. Yes, continue");
        assert_eq!(detect_at_end(Tool::Codex, &s).map(|a| a.0), Some(AttentionKind::Auth));
        let s = screen("Select login method:\n❯ 1. Claude account with subscription");
        assert_eq!(detect_at_end(Tool::Claude, &s).map(|a| a.0), Some(AttentionKind::Auth));
    }

    /// Claude Code 2.1.319 waiting for input (screen of a real run, statusline shortened).
    const CLAUDE_IDLE: &str = " ▐▛███▜▌   Claude Code v2.1.319\n\n\n\
        ────────────────────────────────────────────────────────────\n\
        ❯\u{a0}Try \"refactor <filepath>\"\n\
        ────────────────────────────────────────────────────────────\n\
          user@host:runwick (unknown) | Opus 5.5 [default]\n\
          ⏸ manual mode on";

    /// codex-cli 0.164.0 waiting for input (screen of a real run).
    const CODEX_IDLE: &str = "• OK-RUNWICK\n\n  Worked for 14s • 6:33 PM\n\n\
        › Ask Codex to do anything\n\n\
          GPT-6-Astra xhigh · /private/tmp/repo\n\
          ? for shortcuts                                   ⚠ 8 warnings · f2 to view";

    fn ready(tool: Tool, text: &str) -> bool {
        let s = screen(text);
        ready_for_input(tool, &s, s.len().saturating_sub(1))
    }

    #[test]
    fn idle_input_prompts_are_recognised() {
        assert!(ready(Tool::Claude, CLAUDE_IDLE));
        assert!(ready(Tool::Codex, CODEX_IDLE));
        // Typed text in the box is still the input prompt.
        assert!(ready(Tool::Claude, &CLAUDE_IDLE.replace("Try \"refactor <filepath>\"", "fix the login")));
    }

    #[test]
    fn dialogs_and_unknown_screens_are_not_input() {
        // Choice lists of either CLI, plan approval, a y/n question, an unknown program.
        let trust = "Quick safety check: Is this a project you created or one you trust?\n❯ 1. Yes, I trust this folder\n  2. No, exit";
        assert!(!ready(Tool::Claude, trust));
        assert!(!ready(Tool::Codex, CODEX_APPROVAL));
        let plan = "Claude has written up a plan and is ready to execute. Would you like to proceed?\n\
             ❯ 1. Yes, auto-accept edits\n  2. Yes, manually approve edits\n  3. No, keep planning";
        assert!(!ready(Tool::Claude, plan));
        assert!(!ready(Tool::Claude, &format!("{CLAUDE_IDLE}\nOverwrite the file? [y/N]")));
        assert!(!ready(Tool::Claude, "working on it\nready"));
        assert!(!ready(Tool::Codex, "Starting…"));
        // An earlier input box with a program's own output below it is not the input.
        let below: String = (0..8).map(|i| format!("\nprogram output {i}")).collect();
        assert!(!ready(Tool::Claude, &format!("{CLAUDE_IDLE}{below}")));
        assert!(!ready(Tool::Shell, CLAUDE_IDLE));
        // A numbered list in an answer is not a dialog.
        assert!(ready(Tool::Claude, &CLAUDE_IDLE.replace("\n\n\n", "\n1. first step\n2. second step\n")));
    }

    #[test]
    fn plan_approval_mcp_and_plugin_prompts_need_the_person() {
        let plan = screen(
            "Claude has written up a plan and is ready to execute. Would you like to proceed?\n\
             ❯ 1. Yes, auto-accept edits\n  2. Yes, manually approve edits\n  3. No, keep planning",
        );
        assert_eq!(detect_at_end(Tool::Claude, &plan).map(|a| a.0), Some(AttentionKind::Approval));
        let mcp = screen(
            "New MCP server found in this project: github\n\
             ❯ 1. Use this and all future MCP servers in this project\n  2. Use this MCP server\n  3. Continue without using this MCP server",
        );
        assert_eq!(detect_at_end(Tool::Claude, &mcp).map(|a| a.0), Some(AttentionKind::Approval));
        let several =
            screen("3 new MCP servers found in this project\nSelect any you wish to enable.\n❯ ◯ github\n  ◯ linear");
        assert_eq!(detect_at_end(Tool::Claude, &several).map(|a| a.0), Some(AttentionKind::Approval));
        let plugin =
            screen("The rust-analyzer LSP plugin is available. Would you like to install it?\n❯ Install\n  Not now");
        assert_eq!(detect_at_end(Tool::Claude, &plugin).map(|a| a.0), Some(AttentionKind::Approval));
    }

    /// codex-cli 0.164.0 on its update menu (screen of a real run).
    const CODEX_UPDATE: &str = "\n  Update available · 0.164.0 → 0.167.0\n\
          Release notes: https://github.com/openai/codex/releases/latest\n\n\
        › 1. Update now (runs `brew upgrade --cask codex`)\n\
          2. Skip\n\
          3. Skip until next version\n\n\
          enter continue · esc skip";

    #[test]
    fn a_numbered_menu_at_the_bottom_needs_a_choice() {
        let s = screen(CODEX_UPDATE);
        assert_eq!(
            detect_at_end(Tool::Codex, &s),
            Some((AttentionKind::Choice, "Update available · 0.164.0 → 0.167.0".into()))
        );
        assert!(!ready(Tool::Codex, CODEX_UPDATE), "delivery holds on it");
        // The selection moved, or the menu has no heading.
        let moved = screen(&CODEX_UPDATE.replace("› 1.", "1.").replace("\n2. Skip\n", "\n› 2. Skip\n"));
        assert_eq!(detect_at_end(Tool::Codex, &moved).map(|a| a.0), Some(AttentionKind::Choice));
        let bare = screen("❯ 1. Light\n  2. Dark");
        assert_eq!(detect_at_end(Tool::Claude, &bare), Some((AttentionKind::Choice, "1. Light".into())));
        // Known prompts keep their own kind.
        assert_eq!(detect_at_end(Tool::Codex, &screen(CODEX_APPROVAL)).map(|a| a.0), Some(AttentionKind::Approval));
    }

    #[test]
    fn numbered_text_that_is_not_a_menu_needs_nothing() {
        // A single option; work in progress; a past message above the input prompt; text typed
        // into Claude Code's input box; idle screens; a numbered list in an answer.
        assert_eq!(detect_at_end(Tool::Codex, &screen("› 1. Update now\n\n  enter continue")), None);
        let busy = format!("{CODEX_UPDATE}\n• Working (3s • esc to interrupt)");
        assert_eq!(detect_at_end(Tool::Codex, &screen(&busy)), None);
        let history = format!("› 1. fix login\n  2. add tests\n\n• Done.\n\n{CODEX_IDLE}");
        assert_eq!(detect_at_end(Tool::Codex, &screen(&history)), None);
        let typed = CLAUDE_IDLE.replace("❯\u{a0}Try \"refactor <filepath>\"", "❯ 1. fix login\n  2. add tests");
        assert_eq!(detect_at_end(Tool::Claude, &screen(&typed)), None);
        assert_eq!(detect_at_end(Tool::Claude, &screen(CLAUDE_IDLE)), None);
        assert_eq!(detect_at_end(Tool::Codex, &screen(CODEX_IDLE)), None);
        let answer = CLAUDE_IDLE.replace("\n\n\n", "\n1. first step\n2. second step\n");
        assert_eq!(detect_at_end(Tool::Claude, &screen(&answer)), None);
        // Output went on far below an old menu.
        let below: String = (0..8).map(|i| format!("\noutput {i}")).collect();
        assert_eq!(detect_at_end(Tool::Codex, &screen(&format!("{CODEX_UPDATE}{below}"))), None);
    }

    #[test]
    fn resume_failures_are_counted_per_cli() {
        let claude = "$ claude --resume 1\nNo conversation found with session ID: 1\n";
        assert_eq!(resume_failures(Tool::Claude, claude), 1);
        assert_eq!(resume_failures(Tool::Codex, "Error: no rollout found for thread id 9"), 1);
        assert_eq!(resume_failures(Tool::Shell, claude), 0);
    }

    #[test]
    fn shells_are_never_guessed() {
        let s = screen("Do you want to proceed?\n1. Yes");
        assert_eq!(detect_at_end(Tool::Shell, &s), None);
    }
}

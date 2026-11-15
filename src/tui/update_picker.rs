//! Startup prompt offering an available release, mirroring Codex's picker.
//!
//! Presentation follows Codex: one blank line, a bold headline with the
//! version transition, a dim release-notes line with an underlined URL, the
//! numbered choices, and a dim key-hint line — all inset by two cells.

use super::{picker, render::Frame};
use crate::theme::{self, ColorMode, Rgb, Theme};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// Published notes for the release the banner advertises.
pub const RELEASE_NOTES_URL: &str = "https://github.com/jaddd14/gitcode-agent/releases/latest";

/// What the operator chose in the update banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Run the verified self-update now.
    UpdateNow,
    /// Ask again on the next start.
    Later,
    /// Never offer this exact version again.
    SkipVersion,
}

impl Choice {
    fn next(self) -> Self {
        match self {
            Self::UpdateNow => Self::Later,
            Self::Later => Self::SkipVersion,
            Self::SkipVersion => Self::UpdateNow,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::UpdateNow => Self::SkipVersion,
            Self::Later => Self::UpdateNow,
            Self::SkipVersion => Self::Later,
        }
    }

    #[cfg(test)]
    fn index(self) -> usize {
        match self {
            Self::UpdateNow => 0,
            Self::Later => 1,
            Self::SkipVersion => 2,
        }
    }
}

/// Underline a styled cell run, matching Codex's release-notes link.
fn underlined(value: &str, width: usize, color: Rgb, mode: ColorMode) -> String {
    let value = picker::text(value, width, color, mode);
    if mode == ColorMode::None {
        value
    } else {
        format!("\x1b[4m{value}{}", theme::reset(mode))
    }
}

/// Content lines above the choices; keeps the notes line separable.
pub fn frame(
    selected: Choice,
    installed: &str,
    latest: &str,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Frame {
    let theme = Theme::default();
    let width = width.max(1);
    let heading = format!(
        "{}  {}",
        picker::heading("  Update available", width, &theme, mode),
        picker::text(
            &format!("· {installed} → {latest}"),
            width,
            theme.placeholder,
            mode,
        ),
    );
    let prefix = "  Release notes: ";
    let notes = format!(
        "{}{}",
        picker::text(prefix, width, theme.placeholder, mode),
        underlined(
            RELEASE_NOTES_URL,
            width.saturating_sub(prefix.len()),
            theme.placeholder,
            mode
        ),
    );
    // Keep the highlighted row, then the headline and the key hint, then the
    // remaining choices; notes and spacers yield first when the terminal is
    // too short to show everything.
    let choices = [
        (Choice::UpdateNow, "1. Обновить сейчас (`gitcode update`)"),
        (Choice::Later, "2. Позже"),
        (Choice::SkipVersion, "3. Пропустить до следующей версии"),
    ];
    let mut rows: Vec<(u8, String)> = vec![
        (5, String::new()),
        (1, heading),
        (4, notes),
        (5, String::new()),
    ];
    for (choice, label) in choices {
        let priority = if choice == selected { 0 } else { 3 };
        rows.push((
            priority,
            picker::choice(choice == selected, label, "", width, &theme, mode),
        ));
    }
    rows.push((5, String::new()));
    rows.push((
        2,
        format!(
            "{}{}{}",
            picker::text("  enter подтвердить ", width, theme.placeholder, mode),
            picker::text("·", width, theme.placeholder, mode),
            picker::text(" esc пропустить", width, theme.placeholder, mode),
        ),
    ));
    let limit = height.max(1).min(rows.len());
    let mut ranked: Vec<usize> = (0..rows.len()).collect();
    ranked.sort_by_key(|index| (rows[*index].0, *index));
    ranked.truncate(limit);
    ranked.sort_unstable();
    let lines: Vec<String> = ranked
        .into_iter()
        .map(|index| rows[index].1.clone())
        .collect();
    Frame {
        lines: picker::constrain(lines, height),
        cursor: None,
    }
}

/// Read a key as a menu decision; `None` keeps the banner open.
pub fn answer(event: &Event, selected: &mut Choice) -> Option<Choice> {
    let Event::Key(key) = event else { return None };
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let blocked = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
    match key.code {
        KeyCode::Up | KeyCode::BackTab => {
            *selected = selected.previous();
            None
        }
        KeyCode::Down | KeyCode::Tab => {
            *selected = selected.next();
            None
        }
        KeyCode::Enter => Some(*selected),
        KeyCode::Esc => Some(Choice::Later),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Choice::Later),
        KeyCode::Char('1') if !blocked => Some(Choice::UpdateNow),
        KeyCode::Char('2') if !blocked => Some(Choice::Later),
        KeyCode::Char('3') if !blocked => Some(Choice::SkipVersion),
        KeyCode::Char('u' | 'U' | 'г' | 'Г') if !blocked => Some(Choice::UpdateNow),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use unicode_width::UnicodeWidthStr;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn banner_shows_versions_notes_and_choices() {
        let frame = frame(
            Choice::UpdateNow,
            "0.4.0",
            "0.5.0",
            100,
            20,
            ColorMode::None,
        );
        let content = frame.lines.join("\n");
        assert!(content.contains("Update available"));
        assert!(content.contains("· 0.4.0 → 0.5.0"));
        assert!(content.contains("Release notes:"));
        assert!(content.contains("› 1. Обновить сейчас"));
        assert!(content.contains("2. Позже"));
        assert!(content.contains("3. Пропустить"));
        assert!(content.contains("esc пропустить"));
        assert!(!content.contains('╭'));
        assert!(frame.lines.iter().all(|line| line.width() <= 100));
        // The headline sits after one blank line, as in Codex.
        assert_eq!(frame.lines[0].trim(), "");
        assert!(frame.lines[1].contains("Update available"));
    }

    #[test]
    fn highlighted_row_survives_a_short_terminal() {
        for height in 3..=9 {
            for choice in [Choice::UpdateNow, Choice::Later, Choice::SkipVersion] {
                let frame = frame(choice, "0.4.0", "0.5.0", 40, height, ColorMode::None);
                assert!(frame.lines.len() <= height, "{height} {choice:?}");
                let marker = format!("› {}.", choice.index() + 1);
                assert!(
                    frame.lines.join("\n").contains(&marker),
                    "{height} {choice:?}"
                );
            }
        }
    }

    #[test]
    fn navigation_wraps_and_keys_select() {
        let mut choice = Choice::UpdateNow;
        assert_eq!(answer(&key(KeyCode::Up), &mut choice), None);
        assert_eq!(choice, Choice::SkipVersion);
        assert_eq!(answer(&key(KeyCode::Down), &mut choice), None);
        assert_eq!(choice, Choice::UpdateNow);
        assert_eq!(answer(&key(KeyCode::Down), &mut choice), None);
        assert_eq!(
            answer(&key(KeyCode::Enter), &mut choice),
            Some(Choice::Later)
        );
        assert_eq!(answer(&key(KeyCode::Esc), &mut choice), Some(Choice::Later));
        assert_eq!(
            answer(&key(KeyCode::Char('3')), &mut choice),
            Some(Choice::SkipVersion)
        );
        assert_eq!(answer(&key(KeyCode::Char('x')), &mut choice), None);
        let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(answer(&ctrl_c, &mut choice), Some(Choice::Later));
    }
}

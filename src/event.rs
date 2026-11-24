use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};

/// Semantic actions, independent of whether they came from a prefix chord,
/// a direct key, or a mouse event.
///
/// Navigation is deliberately *layer agnostic*: `Vim(c)` just reports which
/// of h/j/k/l was pressed. `App` decides whether that means "pick another
/// pane" (PANES layer) or "move inside this pane" (CONTENT layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// A key-up / auto-repeat pseudo-event. Must be dropped before the state
    /// machine sees it, otherwise one physical keystroke fires twice and a
    /// released prefix key consumes its own pending prefix.
    Ignored,
    /// (stop all managed processes before leaving?)
    Quit(bool),
    Prefix,
    Help,
    Goto,
    /// h / j / k / l (arrow keys are mapped to the same characters).
    Vim(char),
    /// Relative movement, used by text editing and by programmatic callers.
    Move(i32),
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    FilterMode,
    FilterChar(char),
    FilterBackspace,
    StatusFilter(char),
    Stop,
    Restart,
    Refresh,
    Focus(u8), // 0 = sidebar, 1 = list, 2 = log
    ToggleSidebar,
    ToggleLog,
    CloseLog,
    Zoom,
    NextView,
    PrevView,
    View(usize),
    JdkPicker,
    MavenPicker,
    ConfigPicker,
    NextConfig,
    CopyMode,
    /// `v`: start/end a visual line selection in the log pane.
    Visual,
    /// `y`: yank the current line (or the visual selection).
    CopyLine,
    /// `Y`: yank every line of the log.
    CopyAll,
    Mouse(MouseEvent),
}

fn is_ctrl(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
}

fn plain(k: &KeyEvent) -> bool {
    !k.modifiers.intersects(
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER,
    )
}

pub fn translate(ev: Event, prefix_active: bool, editing: bool, prefix_char: char) -> Action {
    match ev {
        // Win32 reports key-down (Press) AND key-up (Release) for every keystroke;
        // keyboard-enhancement terminals additionally emit Repeat while held.
        // Only the initial press is a user action.
        Event::Key(k) if !matches!(k.kind, KeyEventKind::Press) => Action::Ignored,
        Event::Mouse(m) if matches!(m.kind, MouseEventKind::Moved) => Action::Ignored,
        Event::Mouse(m) => Action::Mouse(m),
        Event::Resize(_, _) => Action::None,
        Event::Key(k) => translate_key(k, prefix_active, editing, prefix_char),
        _ => Action::None,
    }
}

fn translate_key(k: KeyEvent, prefix_active: bool, editing: bool, prefix_char: char) -> Action {
    // Ctrl+C always leaves, even mid-edit or mid-prefix.
    if is_ctrl(&k) && matches!(k.code, KeyCode::Char('c')) {
        return Action::Quit(false);
    }

    if editing {
        return match k.code {
            KeyCode::Esc => Action::Esc,
            KeyCode::Enter => Action::Enter,
            KeyCode::Backspace => Action::FilterBackspace,
            KeyCode::Char(c) if plain(&k) || k.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::FilterChar(c)
            }
            KeyCode::Left => Action::Move(-1),
            KeyCode::Right => Action::Move(1),
            KeyCode::Up => Action::Move(-1),
            KeyCode::Down => Action::Move(1),
            _ => Action::None,
        };
    }

    // The prefix reserves exactly one chord; the next key belongs to the TUI,
    // never to the process running inside the log pane.
    if prefix_active {
        return match k.code {
            KeyCode::Char('?') => Action::Help,
            KeyCode::Char('q') => Action::Quit(false),
            KeyCode::Char('Q') => Action::Quit(true),
            KeyCode::Char('g') => Action::Goto,
            KeyCode::Char('h') => Action::Focus(0),
            KeyCode::Char('l') => Action::Focus(1),
            KeyCode::Char('j') => Action::Focus(2),
            KeyCode::Char('k') => Action::Focus(1),
            KeyCode::Char('1') => Action::Focus(0),
            KeyCode::Char('2') => Action::Focus(1),
            KeyCode::Char('3') => Action::Focus(2),
            KeyCode::Char('v') | KeyCode::Char('-') => Action::ToggleLog,
            KeyCode::Char('x') => Action::CloseLog,
            KeyCode::Char('z') => Action::Zoom,
            KeyCode::Char('b') => Action::ToggleSidebar,
            KeyCode::Char('w') | KeyCode::Char('n') => Action::NextView,
            KeyCode::Char('p') => Action::PrevView,
            KeyCode::Char(c @ '4'..='9') => Action::View((c as usize) - ('0' as usize) - 1),
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('[') => Action::CopyMode,
            KeyCode::Esc => Action::Esc,
            _ => Action::None,
        };
    }

    if is_ctrl(&k) {
        if let KeyCode::Char(c) = k.code
            && c.eq_ignore_ascii_case(&prefix_char)
        {
            return Action::Prefix;
        }
        return match k.code {
            KeyCode::Char('f') => Action::PageDown,
            KeyCode::Char('d') => Action::PageDown,
            KeyCode::Char('u') => Action::PageUp,
            KeyCode::Char('b') => Action::PageUp,
            _ => Action::None,
        };
    }

    match k.code {
        // h/j/k/l and arrows share one action; App interprets them per layer.
        KeyCode::Char('h') | KeyCode::Left => Action::Vim('h'),
        KeyCode::Char('j') | KeyCode::Down => Action::Vim('j'),
        KeyCode::Char('k') | KeyCode::Up => Action::Vim('k'),
        KeyCode::Char('l') | KeyCode::Right => Action::Vim('l'),
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::Home => Action::Home,
        KeyCode::End | KeyCode::Char('G') => Action::End,
        KeyCode::Enter => Action::Enter,
        KeyCode::Esc => Action::Esc,
        KeyCode::Backspace => Action::FilterBackspace,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('/') => Action::FilterMode,
        KeyCode::Char('g') => Action::Goto,
        KeyCode::Char('q') => Action::Quit(false),
        KeyCode::Char('Q') => Action::Quit(true),
        KeyCode::Char('s') | KeyCode::Char('x') => Action::Stop,
        KeyCode::Char('R') => Action::Restart,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('J') => Action::JdkPicker,
        KeyCode::Char('M') => Action::MavenPicker,
        KeyCode::Char('c') => Action::ConfigPicker,
        KeyCode::Char('v') => Action::Visual,
        // Full screen the window under the cursor; App decides per layer.
        KeyCode::Char('z') => Action::Vim('z'),
        KeyCode::Char('y') => Action::CopyLine,
        KeyCode::Char('Y') => Action::CopyAll,
        KeyCode::Char('F') => Action::End, // back to live tail
        KeyCode::Tab => Action::NextConfig,
        KeyCode::Char(c @ ('a' | 'w' | 'b' | 'i' | 'd')) => Action::StatusFilter(c),
        _ => Action::None,
    }
}

pub fn parse_prefix(s: &str) -> char {
    let lower = s.trim().to_lowercase();
    for pat in ["ctrl+", "control+", "^"] {
        if let Some(rest) = lower.strip_prefix(pat) {
            return rest.chars().next().unwrap_or('b');
        }
    }
    'b'
}

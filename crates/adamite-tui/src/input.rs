use ratatui::crossterm::event::{self, KeyCode, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Parent,
    Quit,
}

impl Key {
    fn is_movement(self) -> bool {
        matches!(
            self,
            Self::Up | Self::Down | Self::PageUp | Self::PageDown | Self::Home | Self::End
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Key(Key),
}

impl Event {
    pub(crate) fn from_terminal(event: event::Event) -> Option<Self> {
        let event::Event::Key(event) = event else {
            return None;
        };
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let key = if event.modifiers == KeyModifiers::CONTROL {
            match event.code {
                KeyCode::Char('c' | 'C') => Key::Quit,
                _ => return None,
            }
        } else if event.modifiers.is_empty() || event.modifiers == KeyModifiers::SHIFT {
            match event.code {
                KeyCode::Up | KeyCode::Char('k') => Key::Up,
                KeyCode::Down | KeyCode::Char('j') => Key::Down,
                KeyCode::PageUp => Key::PageUp,
                KeyCode::PageDown => Key::PageDown,
                KeyCode::Home => Key::Home,
                KeyCode::End => Key::End,
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Key::Enter,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => Key::Parent,
                KeyCode::Char('q') | KeyCode::Esc => Key::Quit,
                _ => return None,
            }
        } else {
            return None;
        };
        if event.kind == KeyEventKind::Repeat && !key.is_movement() {
            return None;
        }
        Some(Self::Key(key))
    }
}

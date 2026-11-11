use crate::app::Tab;
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
    Open,
    Parent,
    NextTab,
    SelectTab(Tab),
    Bytes,
    Help,
    Escape,
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
    Resize { width: u16, height: u16 },
}

impl Event {
    pub(crate) fn from_terminal(event: event::Event) -> Option<Self> {
        if let event::Event::Resize(width, height) = event {
            return Some(Self::Resize { width, height });
        }
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
                KeyCode::Enter => Key::Enter,
                KeyCode::Right | KeyCode::Char('l') => Key::Open,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => Key::Parent,
                KeyCode::Tab => Key::NextTab,
                KeyCode::Char('1') => Key::SelectTab(Tab::Tree),
                KeyCode::Char('2') => Key::SelectTab(Tab::TopFiles),
                KeyCode::Char('3') => Key::SelectTab(Tab::Extensions),
                KeyCode::Char('4') => Key::SelectTab(Tab::Info),
                KeyCode::Char('b') => Key::Bytes,
                KeyCode::Char('?') => Key::Help,
                KeyCode::Esc => Key::Escape,
                KeyCode::Char('q') => Key::Quit,
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

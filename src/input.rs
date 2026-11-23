use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

/// A single-line text field with a character cursor and horizontal viewport.
#[derive(Clone, Debug, Default)]
pub struct TextInput {
    pub value: String,
    pub cursor: usize,
}

impl From<String> for TextInput {
    fn from(value: String) -> Self {
        Self {
            cursor: value.chars().count(),
            value,
        }
    }
}

impl TextInput {
    pub fn replace(&mut self, value: String) {
        *self = value.into();
    }

    pub fn insert(&mut self, text: &str) {
        let inserted: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        let mut chars: Vec<char> = self.value.chars().collect();
        self.cursor = self.cursor.min(chars.len());
        chars.splice(self.cursor..self.cursor, inserted.iter().copied());
        self.cursor += inserted.len();
        self.value = chars.into_iter().collect();
    }

    pub fn key(&mut self, key: KeyEvent) {
        let mut chars: Vec<char> = self.value.chars().collect();
        self.cursor = self.cursor.min(chars.len());
        match key.code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = chars.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                chars.remove(self.cursor - 1);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < chars.len() => {
                chars.remove(self.cursor);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                chars.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                chars.insert(self.cursor, c);
                self.cursor += 1;
            }
            _ => {}
        }
        self.value = chars.into_iter().collect();
    }

    pub fn viewport(&self, width: usize) -> (String, u16) {
        if width == 0 {
            return (String::new(), 0);
        }
        let chars: Vec<char> = self.value.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let cell_width =
            |start, end| Line::from(chars[start..end].iter().collect::<String>()).width();
        let mut start = 0;
        while start < cursor && cell_width(start, cursor) >= width {
            start += 1;
        }
        let mut end = start;
        while end < chars.len() && cell_width(start, end + 1) <= width {
            end += 1;
        }
        (
            chars[start..end].iter().collect(),
            cell_width(start, cursor) as u16,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_coordinate_editing_inserts_deletes_and_scrolls_at_cursor() {
        let mut input = TextInput::from("31°13′49.44″N".to_owned());
        input.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        input.insert("+");
        input.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        input.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        input.insert("2");
        assert_eq!(input.value, "+32°13′49.44″N");
        input.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let (visible, column) = input.viewport(8);
        assert!(visible.ends_with('N'));
        assert!(column < 8);
        input.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));
        assert_eq!(input.value, "+32°13′49.44″N");
    }
}

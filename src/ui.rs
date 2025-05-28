use crossterm::{
    cursor::{MoveTo, Show, Hide},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, Write};
use tokio::sync::broadcast;

pub struct AttachUI {
    terminal_height: u16,
    terminal_width: u16,
    output_lines: Vec<String>,
    input_buffer: String,
    cursor_pos: usize,
    max_output_lines: usize,
}

impl AttachUI {
    pub fn new() -> io::Result<Self> {
        let (width, height) = terminal::size()?;
        
        Ok(Self {
            terminal_height: height,
            terminal_width: width,
            output_lines: Vec::new(),
            input_buffer: String::new(),
            cursor_pos: 0,
            max_output_lines: (height as usize).saturating_sub(3), // Reserve 3 lines for input area
        })
    }

    pub fn enter_attach_mode(&mut self, instance_id: &str) -> io::Result<()> {
        // Enter alternate screen and hide cursor
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        terminal::enable_raw_mode()?;
        
        self.clear_screen()?;
        self.draw_header(instance_id)?;
        self.draw_input_area()?;
        
        Ok(())
    }

    pub fn exit_attach_mode(&mut self) -> io::Result<()> {
        // Restore terminal
        terminal::disable_raw_mode()?;
        execute!(io::stdout(), Show, LeaveAlternateScreen)?;
        
        Ok(())
    }

    pub fn add_output_line(&mut self, line: String) -> io::Result<()> {
        self.output_lines.push(line);
        
        // Keep only the last N lines to prevent memory issues
        if self.output_lines.len() > self.max_output_lines * 2 {
            self.output_lines.drain(0..self.max_output_lines);
        }
        
        self.redraw_output()?;
        Ok(())
    }

    pub fn handle_input(&mut self) -> io::Result<Option<String>> {
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key_event) = event::read()? {
                match key_event {
                    KeyEvent {
                        code: KeyCode::Enter,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        let input = self.input_buffer.clone();
                        self.input_buffer.clear();
                        self.cursor_pos = 0;
                        self.draw_input_area()?;
                        return Ok(Some(input));
                    }
                    KeyEvent {
                        code: KeyCode::Char(c),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        self.input_buffer.insert(self.cursor_pos, c);
                        self.cursor_pos += 1;
                        self.draw_input_area()?;
                    }
                    KeyEvent {
                        code: KeyCode::Backspace,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if self.cursor_pos > 0 {
                            self.cursor_pos -= 1;
                            self.input_buffer.remove(self.cursor_pos);
                            self.draw_input_area()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Left,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if self.cursor_pos > 0 {
                            self.cursor_pos -= 1;
                            self.draw_input_area()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Right,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if self.cursor_pos < self.input_buffer.len() {
                            self.cursor_pos += 1;
                            self.draw_input_area()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => {
                        return Ok(Some("detach".to_string()));
                    }
                    _ => {}
                }
            }
        }
        Ok(None)
    }

    fn clear_screen(&mut self) -> io::Result<()> {
        execute!(io::stdout(), Clear(ClearType::All))?;
        Ok(())
    }

    fn draw_header(&mut self, instance_id: &str) -> io::Result<()> {
        queue!(
            io::stdout(),
            MoveTo(0, 0),
            SetForegroundColor(Color::Cyan),
            Print(format!("┌─ Attached to instance: {} ", instance_id)),
        )?;
        
        // Fill the rest of the line with dashes
        let header_len = format!("┌─ Attached to instance: {} ", instance_id).len();
        let remaining = (self.terminal_width as usize).saturating_sub(header_len + 1);
        queue!(io::stdout(), Print("─".repeat(remaining)))?;
        queue!(io::stdout(), Print("┐"))?;
        
        queue!(
            io::stdout(),
            MoveTo(0, 1),
            Print("│ Type 'detach' or Ctrl+C to exit attach mode"),
        )?;
        
        let spaces = (self.terminal_width as usize).saturating_sub(45);
        queue!(io::stdout(), Print(" ".repeat(spaces)))?;
        queue!(io::stdout(), Print("│"))?;
        
        queue!(
            io::stdout(),
            MoveTo(0, 2),
            Print("├"),
            Print("─".repeat((self.terminal_width as usize).saturating_sub(2))),
            Print("┤"),
            ResetColor,
        )?;
        
        io::stdout().flush()?;
        Ok(())
    }

    fn redraw_output(&mut self) -> io::Result<()> {
        let output_start_line = 3;
        let output_end_line = self.terminal_height.saturating_sub(3);
        let available_lines = (output_end_line - output_start_line) as usize;
        
        // Clear output area
        for line in output_start_line..output_end_line {
            queue!(
                io::stdout(),
                MoveTo(0, line),
                Clear(ClearType::CurrentLine),
                Print("│"),
                MoveTo(self.terminal_width - 1, line),
                Print("│"),
            )?;
        }
        
        // Display the last N lines of output
        let start_idx = if self.output_lines.len() > available_lines {
            self.output_lines.len() - available_lines
        } else {
            0
        };
        
        for (i, line) in self.output_lines[start_idx..].iter().enumerate() {
            let y = output_start_line + i as u16;
            if y >= output_end_line {
                break;
            }
            
            queue!(io::stdout(), MoveTo(2, y))?;
            
            // Truncate line if it's too long
            let max_width = (self.terminal_width as usize).saturating_sub(4);
            let display_line = if line.len() > max_width {
                format!("{}...", &line[..max_width.saturating_sub(3)])
            } else {
                line.clone()
            };
            
            queue!(io::stdout(), Print(display_line))?;
        }
        
        io::stdout().flush()?;
        Ok(())
    }

    fn draw_input_area(&mut self) -> io::Result<()> {
        let input_line = self.terminal_height - 2;
        let bottom_line = self.terminal_height - 1;
        
        // Draw separator
        queue!(
            io::stdout(),
            MoveTo(0, input_line),
            SetForegroundColor(Color::Cyan),
            Print("├"),
            Print("─".repeat((self.terminal_width as usize).saturating_sub(2))),
            Print("┤"),
            ResetColor,
        )?;
        
        // Draw input line
        queue!(
            io::stdout(),
            MoveTo(0, bottom_line),
            SetForegroundColor(Color::Cyan),
            Print("│ "),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print(&self.input_buffer),
        )?;
        
        // Draw right border
        queue!(
            io::stdout(),
            MoveTo(self.terminal_width - 1, bottom_line),
            SetForegroundColor(Color::Cyan),
            Print("│"),
            ResetColor,
        )?;
        
        // Position cursor
        let cursor_x = 2 + self.cursor_pos as u16;
        queue!(io::stdout(), MoveTo(cursor_x, bottom_line), Show)?;
        
        io::stdout().flush()?;
        Ok(())
    }
}

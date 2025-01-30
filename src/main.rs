use std::{
    io::{BufRead, BufReader, Read, Stdout, Write},
    net::TcpStream,
};

use crossterm::{
    cursor,
    event::{self, Event, KeyModifiers},
    style,
    terminal::{self, disable_raw_mode, enable_raw_mode},
    ExecutableCommand, QueueableCommand,
};

struct CursorPos {
    /// 0 is the top most row
    row: u16,
    column: u16,
}

enum Mode {
    Normal,
    Insert,
}

struct Privmsg {
    name: String,
    content: String,
}

struct IRC {
    connection: BufReader<TcpStream>,
    auth_token: String,
    message_sender: crossbeam::channel::Sender<String>,
}

impl IRC {
    fn new(address: &str, auth_token: &str) -> anyhow::Result<Self> {
        let mut connection = TcpStream::connect(address)?;

        connection.write_all(b"CAP REQ :twitch.tv/membership twitch.tv/tags twitch.tv/commands")?;

        let mut buf_reader = BufReader::new(&mut connection);

        let mut received = String::new();

        buf_reader.read_line(&mut received)?;

        if received
            != ":tmi.twitch.tv CAP * ACK :twitch.tv/membership twitch.tv/tags twitch.tv/commands"
        {
            return Err(anyhow::anyhow!("no ack"));
        }

        let (message_sender, message_receiver) = crossbeam::channel::unbounded::<String>();

        {
            let mut connection = connection.try_clone()?;

            std::thread::spawn(move || {
                for message in message_receiver {
                    connection.write_all(message.as_bytes()).unwrap();
                }
            });
        }

        Ok(Self {
            connection: BufReader::new(connection),
            auth_token: auth_token.to_string(),
            message_sender,
        })
    }

    fn send_message(&mut self, message: String) -> anyhow::Result<()> {
        let privmsg = format!("{message}");
        self.message_sender.send(privmsg)?;

        Ok(())
    }

    fn read_privmsg(&mut self) -> anyhow::Result<Privmsg> {
        todo!()
    }
}

fn main() {
    let mut stdout = std::io::stdout();

    disable_raw_mode().unwrap();
    enable_raw_mode().unwrap();

    stdout
        .execute(terminal::Clear(terminal::ClearType::All))
        .unwrap();

    let (mut total_columns, mut total_rows) = terminal::size().unwrap();

    let mut cursor_pos = CursorPos {
        row: total_rows,
        column: 0,
    };

    let mut chat_lines: Vec<Privmsg> = Vec::new();

    let mut edit_mode = Mode::Normal;
    stdout.execute(cursor::SetCursorStyle::SteadyBlock).unwrap();

    let mut send_message = String::new();

    let mut auth_token = String::new();
    std::io::stdin().read_line(&mut auth_token).unwrap();

    let mut irc = IRC::new("irc://irc.chat.twitch.tv:6667", &auth_token);

    loop {
        (total_columns, total_rows) = terminal::size().unwrap();

        draw(
            &mut stdout,
            &cursor_pos,
            &edit_mode,
            &chat_lines,
            &send_message,
            total_rows,
        )
        .unwrap();

        match event::read().expect("failed to read event") {
            Event::Key(key_event) => match key_event.code {
                event::KeyCode::Esc => {
                    edit_mode = Mode::Normal;
                    stdout.execute(cursor::SetCursorStyle::SteadyBlock).unwrap();
                }

                event::KeyCode::Backspace if matches!(edit_mode, Mode::Insert) => {
                    if (cursor_pos.column as usize) < send_message.len() {
                        send_message.remove(cursor_pos.column.saturating_sub(1) as usize);
                        cursor_pos.column = cursor_pos.column.saturating_sub(1);
                    }
                }

                event::KeyCode::Right if matches!(edit_mode, Mode::Insert) => {
                    cursor_pos.column = (cursor_pos.column + 1)
                        .min(send_message.len() as u16)
                        .min(total_columns);
                }

                event::KeyCode::Left if matches!(edit_mode, Mode::Insert) => {
                    cursor_pos.column = cursor_pos.column.saturating_sub(1);
                }

                event::KeyCode::End if matches!(edit_mode, Mode::Insert) => {
                    cursor_pos.column = send_message.len() as u16;
                }

                event::KeyCode::Char(c) => match c {
                    'q' if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                        break;
                    }

                    'i' if matches!(edit_mode, Mode::Normal) => {
                        edit_mode = Mode::Insert;
                        stdout.queue(cursor::SetCursorStyle::SteadyBar).unwrap();
                        stdout
                            .queue(cursor::MoveTo(total_columns, total_rows))
                            .unwrap();
                        stdout.flush().unwrap();
                    }

                    'h' if matches!(edit_mode, Mode::Normal) => {
                        if cursor_pos.row == total_rows {
                            cursor_pos.column = cursor_pos.column.saturating_sub(1);
                        } else {
                            if let Some(new_pos) = cursor_pos.column.checked_sub(1) {
                                cursor_pos.column = new_pos;
                            } else {
                                // TODO: Handle going to previous line
                                cursor_pos.row = cursor_pos.row.saturating_sub(1);
                                cursor_pos.column = cursor_pos.column.max(
                                    chat_lines[chat_lines.len() - cursor_pos.row as usize]
                                        .content
                                        .len() as u16,
                                );
                            }
                        }
                    }
                    'j' if matches!(edit_mode, Mode::Normal) => {
                        if chat_lines.len() > cursor_pos.row as usize {
                            cursor_pos.row += 1;
                            cursor_pos.column = cursor_pos.column.min(
                                chat_lines[chat_lines.len() - cursor_pos.row as usize]
                                    .content
                                    .len() as u16,
                            )
                        }
                    }
                    'k' if matches!(edit_mode, Mode::Normal) => {
                        if chat_lines.len() > cursor_pos.row as usize {
                            if let Some(new_pos) = cursor_pos.row.checked_sub(1) {
                                cursor_pos.row = new_pos;

                                cursor_pos.column = cursor_pos.column.min(
                                    chat_lines[chat_lines.len() - cursor_pos.row as usize]
                                        .content
                                        .len() as u16,
                                )
                            }
                        }
                    }
                    'l' if matches!(edit_mode, Mode::Normal) => {
                        if cursor_pos.row == total_rows {
                            if send_message.len() > cursor_pos.column as usize {
                                cursor_pos.column += 1;
                            }
                        } else {
                            if chat_lines[cursor_pos.row as usize].content.len()
                                <= cursor_pos.column as usize
                                && chat_lines.len() <= cursor_pos.row as usize
                            {
                                cursor_pos.row += 1;
                                cursor_pos.column = 0;
                            } else {
                                cursor_pos.column += 1;
                            }
                        }
                    }

                    c if matches!(edit_mode, Mode::Insert) => {
                        send_message.insert(cursor_pos.column as usize, c);
                        cursor_pos.column += 1;
                    }

                    _ => {}
                },
                _ => {}
            },
            _ => {}
        }

        stdout.flush().unwrap();
    }

    disable_raw_mode().unwrap();
}

fn draw(
    stdout: &mut Stdout,
    cursor_pos: &CursorPos,
    edit_mode: &Mode,
    chat_messages: &[Privmsg],
    send_message: &str,
    total_rows: u16,
) -> anyhow::Result<()> {
    stdout
        .execute(terminal::Clear(terminal::ClearType::All))
        .unwrap();

    let messages_len = chat_messages.len().saturating_sub(total_rows as usize);
    for message in chat_messages[messages_len..].iter().rev() {
        stdout.queue(style::Print(format!(
            "{}: {}",
            message.name, message.content
        )))?;
        stdout.queue(cursor::MoveDown(1))?;
    }

    stdout.flush()?;

    stdout.queue(cursor::MoveTo(0, total_rows))?;

    stdout.queue(style::Print(send_message))?;

    stdout.queue(cursor::MoveTo(
        cursor_pos.column as u16,
        cursor_pos.row as u16,
    ))?;

    stdout.flush()?;

    Ok(())
}

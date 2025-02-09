use core::panic;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Stdout, Write},
    net::TcpStream,
    time::Duration,
};

use arboard::Clipboard;
use crossterm::{
    cursor,
    event::{self, Event, KeyModifiers},
    style,
    terminal::{self, disable_raw_mode, enable_raw_mode},
    ExecutableCommand, QueueableCommand,
};

use unicode_segmentation::UnicodeSegmentation;

struct CursorPos {
    /// 0 is the top most row
    row: u16,
    column: u16,
}

enum Mode {
    Normal,
    Insert,
    Y,
    D,
}

#[derive(Clone, Debug, Default)]
struct Tags(HashMap<String, String>);

impl Tags {
    fn get(&self, tag: &str) -> Option<&String> {
        self.0.get(tag)
    }

    fn parse(raw_message: &str, pos: &mut usize) -> Option<Self> {
        if raw_message[*pos..].starts_with('@') {
            if let Some(space_index) = raw_message[*pos..].find(' ') {
                let mut map = HashMap::new();

                let message = &raw_message[*pos..space_index];
                for tag in message.split(';') {
                    let (key, value) = tag.split_once('=').unwrap();

                    map.insert(key.to_string(), value.to_string());
                }

                *pos = space_index + 1;

                return Some(Self(map));
            }
        }

        None
    }
}

#[derive(Debug)]
struct Prefix {
    nick: Option<String>,
    user: Option<String>,
    host: String,
}

impl Prefix {
    fn parse(raw_message: &str, pos: &mut usize) -> Option<Self> {
        if raw_message[*pos..].starts_with(':') {
            let host_start = *pos + 1;
            let mut nick = None;
            let mut user = None;
            let host;

            let Some(end_index) = raw_message[*pos..].find(' ') else {
                return None;
            };

            if let Some(user_index) = raw_message[*pos..].find('!') {
                nick = Some(raw_message[host_start..*pos + user_index].to_string());
                let Some(host_start) = raw_message[*pos..].find('@') else {
                    return None;
                };

                user = Some(raw_message[*pos + user_index + 1..*pos + host_start].to_string());
                host = raw_message[*pos + host_start + 1..*pos + end_index].to_string();
            } else {
                host = raw_message[host_start..*pos + end_index].to_string();
            }

            *pos += end_index + 1;

            return Some(Self { nick, user, host });
        }

        None
    }
}

struct Privmsg {
    tags: Tags,
    prefix: Prefix,
    channel: String,
    message: String,
}

impl Privmsg {
    fn message_line(&self) -> String {
        format!(
            "{}: {}",
            self.tags
                .get("display-name")
                .unwrap_or(self.prefix.user.as_ref().unwrap_or(&self.channel)),
            self.message
        )
    }

    fn message_line_len(&self) -> usize {
        self.message_line().graphemes(true).count()
    }
}

#[derive(Debug)]
struct IRCMessage {
    tags: Tags,
    prefix: Prefix,
    command: IRCCommand,
}

impl IRCMessage {
    fn parse(raw_message: &str) -> Option<Self> {
        let mut pos = 0;

        let tags = Tags::parse(raw_message, &mut pos).unwrap_or_default();
        let prefix = Prefix::parse(raw_message, &mut pos)?;
        let command = IRCCommand::parse(raw_message, &mut pos)?;

        Some(Self {
            tags,
            prefix,
            command,
        })
    }
}

#[derive(Debug)]
enum IRCCommand {
    Privmsg { channel: String, message: String },
    GlobalUserState,
    Unknown(String),
    CapAck,
    Ping,
}

impl IRCCommand {
    fn parse(raw_message: &str, pos: &mut usize) -> Option<Self> {
        if let Some(privmsg) = raw_message[*pos..].strip_prefix("PRIVMSG ") {
            let Some(channel_start) = privmsg.find('#') else {
                return None;
            };

            let Some(message_start) = privmsg.find(':') else {
                return None;
            };

            return Some(IRCCommand::Privmsg {
                channel: privmsg[channel_start + 1..message_start - 1].to_string(),
                message: privmsg[message_start + 1..].to_string(),
            });
        }

        if let Some(_) = raw_message[*pos..].strip_prefix("GLOBALUSERSTATE") {
            return Some(IRCCommand::GlobalUserState);
        }

        if let Some(_) = raw_message[*pos..].strip_prefix("CAP * ACK") {
            return Some(IRCCommand::CapAck);
        }

        if let Some(_) = raw_message[*pos..].strip_prefix("PING :tmi.twitch.tv\r\n") {
            return Some(IRCCommand::Ping);
        }

        Some(IRCCommand::Unknown(
            raw_message[*pos..raw_message.len()].to_string(),
        ))
    }
}

struct IRC {
    irc_message_receiver: crossbeam::channel::Receiver<IRCMessage>,
    auth_token: String,
    message_sender: crossbeam::channel::Sender<String>,
    channel: String,
    nick: String,
}

impl IRC {
    fn new(address: &str, auth_token: &str, nick: &str, channel: &str) -> anyhow::Result<Self> {
        let connection = TcpStream::connect(address)?;

        let (message_sender, message_receiver) = crossbeam::channel::unbounded::<String>();

        {
            let mut connection = connection.try_clone()?;

            std::thread::spawn(move || {
                for message in message_receiver {
                    connection.write_all(message.as_bytes()).unwrap();
                }
            });
        }

        let (irc_message_sender, irc_message_receiver) =
            crossbeam::channel::unbounded::<IRCMessage>();

        {
            let mut connection = BufReader::new(connection);
            std::thread::spawn(move || loop {
                let mut buf = String::new();
                while let Ok(bytes_read) = connection.read_line(&mut buf) {
                    if bytes_read > 0 {
                        if let Some(irc_message) = IRCMessage::parse(&buf) {
                            irc_message_sender.send(irc_message).unwrap();
                        }

                        buf.clear();
                    }
                }
            });
        }

        message_sender.send(String::from(
            "CAP REQ :twitch.tv/membership twitch.tv/tags twitch.tv/commands\r\n",
        ))?;

        let received = irc_message_receiver.recv_timeout(Duration::from_secs(5))?;
        if !matches!(
            received,
            IRCMessage {
                tags: _,
                prefix: _,
                command: IRCCommand::CapAck
            },
        ) {
            eprintln!("{received:?}");
            return Err(anyhow::anyhow!("no ack"));
        }

        message_sender.send(format!("PASS oauth:{}\r\n", auth_token))?;

        message_sender.send(format!("NICK {}\r\n", nick))?;

        message_sender.send(format!("JOIN #{channel}\r\n"))?;

        Ok(Self {
            irc_message_receiver,
            auth_token: auth_token.to_string(),
            message_sender,
            channel: channel.to_string(),
            nick: nick.to_string(),
        })
    }

    fn send_message(&mut self, message: &str) -> anyhow::Result<()> {
        let privmsg = format!("PRIVMSG #{} :{message}\r\n", self.channel);
        self.message_sender.send(privmsg)?;

        Ok(())
    }

    fn try_recv(&mut self) -> anyhow::Result<IRCMessage> {
        Ok(self.irc_message_receiver.try_recv()?)
    }
}

fn main() {
    let args = std::env::args().collect::<Vec<String>>();

    let (channel, auth_token) = match &args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [_cmd, "--token", token, "--channel", channel] => (channel.to_string(), token.to_string()),
        [_cmd, "--channel", channel] => (
            channel.to_string(),
            std::env::var("TWITCH_TOKEN").expect("should provide twitch auth token"),
        ),
        _ => {
            panic!("Should provide a channel name")
        }
    };

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

    let mut chat_messages: Vec<Privmsg> = Vec::new();

    let mut edit_mode = Mode::Normal;
    stdout.execute(cursor::SetCursorStyle::SteadyBlock).unwrap();
    stdout
        .execute(event::PushKeyboardEnhancementFlags(
            event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ))
        .unwrap();

    let mut send_message = String::new();

    let mut irc = IRC::new(
        "irc.chat.twitch.tv:6667",
        &auth_token,
        "sadmadladsalman",
        &channel,
    )
    .unwrap();

    let mut user_tags = None;

    let mut clipboard = Clipboard::new().unwrap();

    loop {
        while let Ok(irc_message) = irc.try_recv() {
            match irc_message.command {
                IRCCommand::Privmsg { channel, message } => {
                    chat_messages.push(Privmsg {
                        tags: irc_message.tags,
                        prefix: irc_message.prefix,
                        channel,
                        message,
                    });
                }
                IRCCommand::GlobalUserState => {
                    user_tags = Some(irc_message.tags);
                }
                _ => {}
            }
        }

        (total_columns, total_rows) = terminal::size().unwrap();

        draw(
            &mut stdout,
            &cursor_pos,
            &edit_mode,
            &chat_messages,
            &send_message,
            total_rows,
        )
        .unwrap();

        if event::poll(Duration::from_millis(16)).unwrap() {
            let messages_lines_start_pos = total_rows
                .saturating_sub(chat_messages.len() as u16)
                .saturating_sub(1);

            let current_message_index =
                cursor_pos.row.saturating_sub(messages_lines_start_pos) as usize;

            match event::read().expect("failed to read event") {
                Event::Key(key_event) => match key_event.code {
                    event::KeyCode::Esc => {
                        edit_mode = Mode::Normal;
                        stdout.execute(cursor::SetCursorStyle::SteadyBlock).unwrap();
                    }

                    event::KeyCode::Enter if matches!(edit_mode, Mode::Insert) => {
                        if !send_message.is_empty() {
                            if irc.send_message(&send_message).is_ok() {
                                chat_messages.push(Privmsg {
                                    tags: user_tags.as_ref().cloned().unwrap_or_default(),
                                    prefix: Prefix {
                                        nick: Some(irc.nick.clone()),
                                        user: Some(irc.nick.clone()),
                                        host: String::from("idk"),
                                    },
                                    channel: irc.channel.clone(),
                                    message: send_message.clone(),
                                });

                                if !key_event.modifiers.contains(KeyModifiers::CONTROL) {
                                    send_message.clear();
                                    cursor_pos.column = 0;
                                }
                            }
                        }
                    }

                    event::KeyCode::Backspace if matches!(edit_mode, Mode::Insert) => {
                        if (cursor_pos.column as usize) <= send_message.len()
                            && send_message.len() > 0
                        {
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
                        'c' if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            break;
                        }

                        'i' if matches!(edit_mode, Mode::Normal) => {
                            edit_mode = Mode::Insert;
                            stdout.execute(cursor::SetCursorStyle::SteadyBar).unwrap();
                            if cursor_pos.row < total_rows - 1 {
                                cursor_pos.row = total_rows.saturating_sub(1);
                                cursor_pos.column = send_message.graphemes(true).count() as u16;
                            }
                        }

                        'h' if matches!(edit_mode, Mode::Normal) => {
                            if cursor_pos.row >= total_rows - 1 {
                                cursor_pos.column = cursor_pos.column.saturating_sub(1);
                            } else {
                                if let Some(new_pos) = cursor_pos.column.checked_sub(1) {
                                    cursor_pos.column = new_pos;
                                } else {
                                    if messages_lines_start_pos > cursor_pos.row {
                                        // TODO: Handle going to previous line
                                        cursor_pos.row = cursor_pos.row.saturating_sub(1);
                                        // cursor_pos.column = cursor_pos.column.max(
                                        //     chat_lines[chat_lines.len() - cursor_pos.row as usize]
                                        //         .message
                                        //         .len() as u16,
                                        // );
                                    }
                                }
                            }
                        }
                        'j' if matches!(edit_mode, Mode::Normal) => {
                            cursor_pos.row = (total_rows - 1).min(cursor_pos.row + 1);

                            if cursor_pos.row >= total_rows - 1 {
                                cursor_pos.column = cursor_pos
                                    .column
                                    .min(send_message.graphemes(true).count() as u16);
                            } else {
                                let current_message = chat_messages
                                    .get((cursor_pos.row - messages_lines_start_pos) as usize);

                                let Some(current_message) = current_message else {
                                    continue;
                                };

                                cursor_pos.column = cursor_pos
                                    .column
                                    .min(current_message.message_line_len() as u16);
                            }
                        }
                        'k' if matches!(edit_mode, Mode::Normal) => {
                            if messages_lines_start_pos < cursor_pos.row && chat_messages.len() > 0
                            {
                                if let Some(new_pos) = cursor_pos.row.checked_sub(1) {
                                    cursor_pos.row = new_pos;

                                    // cursor_pos.column = cursor_pos.column.min(
                                    //     chat_lines[chat_lines.len() - cursor_pos.row as usize]
                                    //         .message
                                    //         .len() as u16,
                                    // )
                                }
                                // println!("k: {messages_lines_start_pos}: {}", cursor_pos.row);
                            }
                        }
                        'l' if matches!(edit_mode, Mode::Normal) => {
                            if cursor_pos.row >= total_rows - 1 {
                                if send_message.len() > cursor_pos.column as usize {
                                    cursor_pos.column += 1;
                                }
                            } else {
                                let Some(current_message) =
                                    chat_messages.get(current_message_index)
                                else {
                                    continue;
                                };

                                if current_message.message_line_len() <= cursor_pos.column as usize
                                    && chat_messages.len() <= cursor_pos.row as usize
                                {
                                    cursor_pos.row += 1;
                                    cursor_pos.column = 0;
                                } else {
                                    cursor_pos.column += 1;
                                }
                            }
                        }

                        'b' if matches!(edit_mode, Mode::Normal) => {
                            if cursor_pos.row >= total_rows - 1 {
                                cursor_pos.column =
                                    send_message[..cursor_pos.column.saturating_sub(1) as usize]
                                        .rfind(' ')
                                        .map(|i| i + 1)
                                        .unwrap_or(0) as u16;
                            } else {
                                let Some(current_message) =
                                    chat_messages.get(current_message_index)
                                else {
                                    continue;
                                };

                                cursor_pos.column = current_message.message_line()
                                    [..cursor_pos.column.saturating_sub(1) as usize]
                                    .rfind(' ')
                                    .map(|i| i + 1)
                                    .unwrap_or(0)
                                    as u16;
                            }
                        }
                        'w' if matches!(edit_mode, Mode::Normal) => {
                            if cursor_pos.row >= total_rows - 1 {
                                if let Some(send_message) =
                                    send_message.get((cursor_pos.column + 1) as usize..)
                                {
                                    cursor_pos.column +=
                                        send_message.find(' ').map(|i| i + 1).unwrap_or(
                                            send_message
                                                .graphemes(true)
                                                .count()
                                                .saturating_sub(cursor_pos.column as usize),
                                        ) as u16;
                                }
                            } else {
                                let Some(current_message) =
                                    chat_messages.get(current_message_index)
                                else {
                                    continue;
                                };

                                if let Some(message) = current_message
                                    .message_line()
                                    .get((cursor_pos.column + 1) as usize..)
                                {
                                    cursor_pos.column += message.find(' ').map(|i| i + 1).unwrap_or(
                                        current_message
                                            .message_line_len()
                                            .saturating_sub(cursor_pos.column as usize),
                                    )
                                        as u16;
                                }
                            }
                        }

                        '$' if matches!(edit_mode, Mode::Normal) => {
                            let Some(current_message) = chat_messages.get(current_message_index)
                            else {
                                continue;
                            };

                            cursor_pos.column = current_message.message_line_len() as u16;
                        }

                        '^' if matches!(edit_mode, Mode::Normal) => {
                            cursor_pos.column = 0;
                        }

                        'y' if matches!(edit_mode, Mode::Normal) => {
                            edit_mode = Mode::Y;
                        }

                        'd' if matches!(edit_mode, Mode::Normal) => {
                            edit_mode = Mode::D;
                        }

                        c if matches!(edit_mode, Mode::Y) => {
                            if c == 'y' {
                                if let Some(current_message) =
                                    chat_messages.get(current_message_index)
                                {
                                    clipboard.set_text(&current_message.message).unwrap();
                                };
                            }

                            edit_mode = Mode::Normal;
                        }

                        c if matches!(edit_mode, Mode::D) => {
                            if c == 'd' {
                                if cursor_pos.row == total_rows - 1 {
                                    clipboard.set_text(&send_message).unwrap();
                                    send_message.clear();
                                    cursor_pos.column = 0;
                                }
                            }

                            edit_mode = Mode::Normal;
                        }

                        'P' if matches!(edit_mode, Mode::Normal) => {
                            if let Ok(clipboard_text) = clipboard.get_text() {
                                if cursor_pos.row != total_rows - 1 {
                                    cursor_pos.row = total_rows - 1;
                                    cursor_pos.column = send_message.graphemes(true).count() as u16;
                                }

                                send_message
                                    .insert_str(cursor_pos.column as usize, &clipboard_text);
                                cursor_pos.column += clipboard_text.graphemes(true).count() as u16;
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

    let messages_start = chat_messages.len().saturating_sub(total_rows as usize);
    let first_message_pos = total_rows
        .saturating_sub(chat_messages.len() as u16)
        .saturating_sub(1);
    stdout.queue(cursor::MoveTo(0, first_message_pos))?;
    for (i, message) in chat_messages[messages_start..].iter().enumerate() {
        stdout.queue(style::Print(message.message_line()))?;
        stdout.queue(cursor::MoveTo(0, first_message_pos + i as u16 + 1))?;
    }

    stdout.queue(cursor::MoveTo(0, total_rows))?;

    stdout.queue(style::Print(send_message))?;

    stdout.queue(cursor::MoveTo(
        cursor_pos.column as u16,
        cursor_pos.row as u16,
    ))?;

    stdout.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tags_parsing() {
        let message = "@badge-info=;badges=moderator/1;color=;display-name=bar;emote-sets=0,300374282;mod=1;subscriber=0;user-type=mod :tmi.twitch.tv USERSTATE #foo";
        let mut pos = 0;
        let tags = Tags::parse(message, &mut pos).unwrap();

        eprintln!("{tags:?}");

        assert_eq!(&message[pos - 1..pos], " ");

        assert_eq!(pos, 112);
    }

    #[test]
    fn test_prefix_parsing() {
        let message = "@badge-info=;badges=moderator/1;color=;display-name=bar;emote-sets=0,300374282;mod=1;subscriber=0;user-type=mod :tmi.twitch.tv USERSTATE #foo";
        let mut pos = 0;
        let _ = Tags::parse(message, &mut pos).unwrap();
        let prefix = Prefix::parse(message, &mut pos).unwrap();

        eprintln!("{prefix:?}");

        assert_eq!(&message[pos..pos + 1], "U");
    }

    #[test]
    fn test_prefix_parsing_with_nick_and_user() {
        let message = "@badge-info=;badges=broadcaster/1;client-nonce=28e05b1c83f1e916ca1710c44b014515;color=#0000FF;display-name=foofoo;emotes=62835:0-10;first-msg=0;flags=;id=f80a19d6-e35a-4273-82d0-cd87f614e767;mod=0;room-id=713936733;subscriber=0;tmi-sent-ts=1642696567751;turbo=0;user-id=713936733;user-type= :foofoo!foofoo@foofoo.tmi.twitch.tv PRIVMSG #bar :bleedPurple";
        let mut pos = 0;
        let _ = Tags::parse(message, &mut pos).unwrap();
        let prefix = Prefix::parse(message, &mut pos).unwrap();

        eprintln!("{prefix:?}");

        assert_eq!(&message[pos..pos + 1], "P");
    }

    #[test]
    fn test_command_parsing() {
        let message = "@badge-info=;badges=broadcaster/1;client-nonce=28e05b1c83f1e916ca1710c44b014515;color=#0000FF;display-name=foofoo;emotes=62835:0-10;first-msg=0;flags=;id=f80a19d6-e35a-4273-82d0-cd87f614e767;mod=0;room-id=713936733;subscriber=0;tmi-sent-ts=1642696567751;turbo=0;user-id=713936733;user-type= :foofoo!foofoo@foofoo.tmi.twitch.tv PRIVMSG #bar :bleedPurple";
        let mut pos = 0;
        let _ = Tags::parse(message, &mut pos).unwrap();
        let _ = Prefix::parse(message, &mut pos).unwrap();
        let command = IRCCommand::parse(message, &mut pos).unwrap();

        eprintln!("{command:?}");

        assert!(false);
    }
}

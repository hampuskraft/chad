use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::thread;

use chrono::{TimeZone, Utc};
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
	disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use signal_hook::consts::SIGINT;
use signal_hook::iterator::Signals;
use tui::backend::{Backend, CrosstermBackend};
use tui::layout::Rect;
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tui::Terminal;

#[derive(Serialize, Deserialize)]
struct Index {
	#[serde(flatten)]
	channels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ChannelInfo {
	id: String,
	#[serde(rename = "type")]
	channel_type: String,
}

fn update_value(value: &str) -> String {
	if let Some(rest) = value.strip_prefix("Direct Message with ") {
		return rest.to_string();
	}
	if let Some((text1, text2)) = value.rsplit_once(" in ") {
		return format!("{} - {}", text2, text1);
	}
	value.to_string()
}

fn parse_snowflake_to_timestamp(snowflake: &str) -> String {
	let id: u64 = snowflake.parse().expect("Invalid snowflake ID");
	let timestamp = (id >> 22) + 1420070400000;
	let dt = Utc.timestamp_millis_opt(timestamp as i64).unwrap();
	dt.format("%B %Y").to_string()
}

fn preprocess_index(index: &mut Index) {
	for value in index.channels.values_mut() {
		*value = update_value(value);
	}
	let index_path = "messages/index.json";
	let mut file = File::create(index_path).expect("Failed to create index.json");
	serde_json::to_writer_pretty(&mut file, &index).expect("Failed to write updated index.json");
}

fn load_channels(
) -> Result<Vec<(String, String, String, String, usize, usize, bool)>, Box<dyn Error>> {
	let messages_dir = "messages";
	let mut channels_info = Vec::new();

	let index_path = "messages/index.json";
	let index_file = fs::read_to_string(index_path)?;
	let mut index: Index = serde_json::from_str(&index_file)?;

	preprocess_index(&mut index);

	for (channel_id, channel_name) in &index.channels {
		let channel_dir = format!("{}/c{}", messages_dir, channel_id);
		let channel_info_path = format!("{}/channel.json", channel_dir);
		let messages_file_path = format!("{}/messages.json", channel_dir);

		if Path::new(&channel_info_path).exists() && Path::new(&messages_file_path).exists() {
			let channel_info_content = fs::read_to_string(&channel_info_path)?;
			let channel_info: ChannelInfo = serde_json::from_str(&channel_info_content)?;

			let creation_date = parse_snowflake_to_timestamp(&channel_info.id);

			let messages_content = fs::read_to_string(&messages_file_path)?;
			let messages: Vec<Value> = serde_json::from_str(&messages_content)?;

			let message_count = messages.len();
			let attachment_count = messages
				.iter()
				.filter(|msg| msg["Attachments"] != "")
				.count();

			if message_count > 0 {
				let channel_name = if channel_name.starts_with("DM - ") {
					channel_name.strip_prefix("DM - ").unwrap().to_string()
				} else {
					channel_name.clone()
				};

				channels_info.push((
					channel_info.channel_type.clone(),
					channel_name,
					creation_date,
					channel_info.id.clone(),
					message_count,
					attachment_count,
					true,
				));
			}
		}
	}

	channels_info.sort_by(|a, b| {
		b.4.cmp(&a.4)
			.then_with(|| a.0.cmp(&b.0))
			.then_with(|| a.1.cmp(&b.1))
	});

	Ok(channels_info)
}

fn draw_ui<B: Backend>(
	terminal: &mut Terminal<B>,
	channels: &Vec<(String, String, String, String, usize, usize, bool)>,
	selected_index: usize,
	offset: usize,
	command_mode: bool,
	command_input: &str,
) -> io::Result<()> {
	let col_widths = [8, 30, 15, 10, 12];
	let max_name_length = col_widths[1];

	let headers = format!(
		"{:<width1$} {:<width2$} {:<width3$} {:>width4$} {:>width5$}",
		"Type",
		"Name",
		"Date",
		"Msgs",
		"Attchs",
		width1 = col_widths[0],
		width2 = col_widths[1],
		width3 = col_widths[2],
		width4 = col_widths[3],
		width5 = col_widths[4],
	);

	terminal.draw(|f| {
		let size = f.size();
		let visible_items = size.height as usize - 3;

		let header_area = Rect {
			x: size.x,
			y: size.y,
			width: size.width,
			height: 1,
		};
		f.render_widget(Paragraph::new(headers), header_area);

		let list_area = Rect {
			x: size.x,
			y: size.y + 1,
			width: size.width,
			height: size.height - 3,
		};

		let items: Vec<ListItem> = channels
			.iter()
			.skip(offset)
			.take(visible_items)
			.enumerate()
			.map(
				|(
					i,
					(_, channel_name, creation_date, _, message_count, attachment_count, selected),
				)| {
					let indicator = if *selected { "[x]" } else { "[ ]" };

					let truncated_name = if channel_name.len() > max_name_length {
						format!("{}...", &channel_name[..max_name_length - 3])
					} else {
						channel_name.clone()
					};

					ListItem::new(format!(
						"{:<width1$} {:<width2$} {:<width3$} {:>width4$} {:>width5$}",
						indicator,
						truncated_name,
						creation_date,
						message_count,
						attachment_count,
						width1 = col_widths[0],
						width2 = col_widths[1],
						width3 = col_widths[2],
						width4 = col_widths[3],
						width5 = col_widths[4],
					))
					.style(if i + offset == selected_index {
						Style::default().fg(Color::Yellow)
					} else {
						Style::default()
					})
				},
			)
			.collect();

		f.render_widget(
			List::new(items).block(
				Block::default()
					.title(Spans::from(vec![
						Span::styled(
							"Select Channels to Export",
							Style::default().fg(Color::Magenta),
						),
						Span::raw(format!(" ({}/{})", offset + 1, channels.len())),
					]))
					.borders(Borders::ALL),
			),
			list_area,
		);

		if command_mode {
			let command_area = Rect {
				x: size.x,
				y: size.height - 1,
				width: size.width,
				height: 1,
			};
			f.render_widget(Paragraph::new(format!(":{}", command_input)), command_area);
		}
	})?;

	Ok(())
}

fn export_to_txt(selected_channels: &Vec<(String, String)>) -> io::Result<()> {
	let output_file = "exported_channels.txt";
	let mut channels: BTreeMap<String, Vec<String>> = BTreeMap::new();

	for (channel_id, message_id) in selected_channels {
		channels
			.entry(channel_id.clone())
			.or_insert_with(Vec::new)
			.push(message_id.clone());
	}

	let mut txtfile = File::create(output_file)?;
	for (channel_id, message_ids) in channels {
		writeln!(txtfile, "{}:", channel_id)?;
		writeln!(txtfile, "{}", message_ids.join(", "))?;
		writeln!(txtfile)?;
	}

	println!(
		"Conversion completed. The file has been saved as {}",
		output_file
	);
	Ok(())
}

fn handle_command(
	command: &str,
	channels: &Vec<(String, String, String, String, usize, usize, bool)>,
) -> io::Result<()> {
	match command {
		"export" => {
			let mut selected_channels = Vec::new();
			for (_, _, _, channel_id, _, _, selected) in channels {
				if *selected {
					let messages_file_path = format!("messages/c{}/messages.json", channel_id);
					let messages_content = fs::read_to_string(&messages_file_path)
						.expect("Failed to read messages.json");
					let messages: Vec<Value> = serde_json::from_str(&messages_content)
						.expect("Failed to parse messages.json");

					for msg in messages {
						if let Some(msg_id) = msg["ID"]
							.as_u64()
							.or_else(|| msg["ID"].as_str().map(|s| s.parse::<u64>().unwrap()))
						{
							selected_channels.push((channel_id.clone(), msg_id.to_string()));
						}
					}
				}
			}
			export_to_txt(&selected_channels)?;
			println!("Exported selected channels to exported_channels.txt");
		}
		"exit" | "quit" => {
			disable_raw_mode()?;
			execute!(io::stdout(), LeaveAlternateScreen)?;
			println!("Exiting...");
			std::process::exit(0);
		}
		_ => {
			println!("Unknown command: {}", command);
		}
	}

	Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let running = Arc::new(AtomicBool::new(true));
	let r = running.clone();

	let mut signals = Signals::new(&[SIGINT])?;
	thread::spawn(move || {
		for _ in signals.forever() {
			r.store(false, AtomicOrdering::SeqCst);
		}
	});

	let mut channels = load_channels().unwrap();
	let mut selected_index = 0;
	let mut offset = 0;

	let stdout = io::stdout();
	let backend = CrosstermBackend::new(stdout);
	let mut terminal = Terminal::new(backend)?;

	terminal.clear()?;
	execute!(terminal.backend_mut(), EnterAlternateScreen)?;

	enable_raw_mode()?;

	let mut command_mode = false;
	let mut command_input = String::new();

	while running.load(AtomicOrdering::SeqCst) {
		draw_ui(
			&mut terminal,
			&channels,
			selected_index,
			offset,
			command_mode,
			&command_input,
		)?;

		if let Event::Key(key) = event::read()? {
			if command_mode {
				match key.code {
					KeyCode::Char(c) => {
						command_input.push(c);
					}
					KeyCode::Backspace => {
						command_input.pop();
					}
					KeyCode::Enter => {
						handle_command(&command_input, &channels)?;
						command_mode = false;
						command_input.clear();
					}
					KeyCode::Esc => {
						command_mode = false;
						command_input.clear();
					}
					_ => {}
				}
			} else {
				match key.code {
					KeyCode::Char(':') => {
						command_mode = true;
					}
					KeyCode::Up => {
						if selected_index > 0 {
							selected_index -= 1;
							if selected_index < offset {
								offset -= 1;
							}
						}
					}
					KeyCode::Down => {
						if selected_index < channels.len() - 1 {
							selected_index += 1;
							if selected_index >= offset + (terminal.size()?.height as usize - 3) {
								offset += 1;
							}
						}
					}
					KeyCode::Char(' ') => {
						channels[selected_index].6 = !channels[selected_index].6;
					}
					KeyCode::Esc => {
						break;
					}
					_ => {}
				}
			}
		}
	}

	disable_raw_mode()?;
	execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
	terminal.clear()?;
	println!("Exited the application");

	Ok(())
}

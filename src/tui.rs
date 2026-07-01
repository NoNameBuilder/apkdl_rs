use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use reqwest::blocking::Client;

use crate::config::Config;
use crate::download::{dl_apkpure, dl_apkmirror, dl_gplay};
use crate::search::{search_play, list_versions_apkmirror, list_versions_apkpure, Version};
use crate::util::{ARCHES, TEMP_PREFIX};
use std::fs;

pub enum Mode {
    Search,
    SelectApp,
    SelectVersion,
    #[allow(dead_code)]
    Downloading,
    #[allow(dead_code)]
    Done,
}

pub struct App {
    pub mode: Mode,
    pub input: String,
    pub search_results: Vec<(String, String)>,
    pub list_state: ListState,
    pub versions: Vec<Version>,
    pub selected_pkg: String,
    pub selected_arch: usize,
    pub log: Vec<String>,
    pub download_ok: bool,
    pub download_msg: String,
    pub quit: bool,
    pub client: Client,
    pub cfg: Config,
}

impl App {
    pub fn new(client: Client, cfg: Config) -> Self {
        Self {
            mode: Mode::Search, input: String::new(), search_results: vec![], list_state: ListState::default(),
            versions: vec![], selected_pkg: String::new(), selected_arch: 0,
            log: vec![], download_ok: false, download_msg: String::new(), quit: false, client, cfg,
        }
    }

    fn do_search(&mut self) {
        let results = search_play(&self.client, &self.input);
        if results.is_empty() { self.log.push("No results found.".into()); return; }
        self.search_results = results;
        self.list_state.select(Some(0));
        self.mode = Mode::SelectApp;
    }

    fn confirm_app(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            if idx < self.search_results.len() { self.selected_pkg = self.search_results[idx].1.clone(); self.do_version_fetch(); }
        }
    }

    fn do_version_fetch(&mut self) {
        self.log.push(format!("Fetching versions for {}...", self.selected_pkg));
        let mut all = vec![];
        if let Ok(v) = list_versions_apkmirror(&self.client, &self.selected_pkg) {
            for mut ver in v { ver.label = format!("[Mirror] {}", ver.label); all.push(ver); }
        }
        if let Ok(v) = list_versions_apkpure(&self.client, &self.selected_pkg) {
            for mut ver in v { ver.label = format!("[Pure] {}", ver.label); all.push(ver); }
        }
        if all.is_empty() { self.versions = vec![Version { label: "(latest)".into(), url: String::new() }]; }
        else { all.insert(0, Version { label: "(latest)".into(), url: String::new() }); self.versions = all; }
        self.list_state.select(Some(0));
        self.mode = Mode::SelectVersion;
    }

    fn start_download(&mut self) {
        let version_url = if let Some(idx) = self.list_state.selected() {
            if idx < self.versions.len() && !self.versions[idx].url.is_empty() {
                Some(self.versions[idx].url.clone())
            } else { None }
        } else { None };
        let arch = ARCHES[self.selected_arch];
        let pkg = self.selected_pkg.clone();
        let client = self.client.clone();
        let out_dir = self.cfg.output_dir.clone().map(PathBuf::from);
        let arch_s = arch.to_string();
        self.log.push(format!("Downloading {} (arch: {})...", pkg, arch));

        // Temp workspace: try system tmp first, fall back to output dir
        let _tmp_guard = tempfile::TempDir::new().ok();
        let tmp_root = match _tmp_guard.as_ref() {
            Some(t) => t.path().to_path_buf(),
            None => out_dir.clone().unwrap_or_else(|| std::env::temp_dir().join(TEMP_PREFIX)),
        };
        fs::create_dir_all(&tmp_root).ok();
        let tmp = tmp_root.join(format!("{}_download_tmp", pkg.replace('.', "_")));
        let out_name = pkg.to_lowercase().replace('.', "_") + ".apk";
        let out = match &out_dir {
            Some(d) => d.join(&out_name),
            None => PathBuf::from(&out_name),
        };

        let sources: [(&str, fn(&Client, &str, &Path, &str, Option<&str>, &mut Vec<String>) -> Result<(), String>); 3] = [
        ("Google Play", dl_gplay),
        ("APKMirror", dl_apkmirror),
        ("APKPure", dl_apkpure),
        ];
        let mut last_err = "no source tried".to_string();
        for (name, func) in &sources {
            let src_tmp = tmp_root.join(format!("{}_tmp", name.replace(' ', "_").to_lowercase()));
            match func(&client, &pkg, &src_tmp, &arch_s, version_url.as_deref(), &mut self.log) {
                Ok(()) => { if src_tmp.exists() { fs::copy(&src_tmp, &tmp).ok(); } last_err.clear(); break; }
                Err(e) => { self.log.push(format!("{name}: {e}")); last_err = e; }
            }
        }
        let result = if last_err.is_empty() {
            if let Some(p) = out.parent() { let _ = fs::create_dir_all(p); }
            if let Err(e) = fs::copy(&tmp, &out) { self.log.push(format!("copy error: {e}")); }
            let sz = out.metadata().map(|m| m.len()).unwrap_or(0);
            self.log.push(format!("Done! {} ({:.1} MB)", out.display(), sz as f64 / 1_000_000.0));
            (true, out.to_string_lossy().to_string())
        } else {
            self.log.push(format!("Failed: {last_err}"));
            (false, last_err)
        };
        self.download_ok = result.0;
        self.download_msg = result.1;
        self.mode = Mode::Done;
    }

    fn tick(&mut self) {}
}

fn render(app: &mut App, frame: &mut Frame) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(3)]).split(frame.area());

    let title = Paragraph::new(" apkdl v3.0 — APK Downloader ")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::NONE).style(Style::default().bg(Color::Black)));
    frame.render_widget(title, chunks[0]);

    match &app.mode {
        Mode::Search => render_search(app, frame, chunks[1]),
        Mode::SelectApp => render_select_app(app, frame, chunks[1]),
        Mode::SelectVersion => render_select_version(app, frame, chunks[1]),
        Mode::Downloading => render_downloading(app, frame, chunks[1]),
        Mode::Done => render_done(app, frame, chunks[1]),
    }

    let arch = ARCHES[app.selected_arch];
    let status = format!(" arch: {} | Tab: cycle arch | Esc: back | q: quit ", arch);
    let bar = Paragraph::new(status).style(Style::default().fg(Color::Yellow))
        .block(Block::default().style(Style::default().bg(Color::DarkGray)));
    frame.render_widget(bar, chunks[2]);
}

fn render_search(app: &App, frame: &mut Frame, area: Rect) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(area);
    let input = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL).title(" Search app name or package "));
    frame.render_widget(input, chunks[0]);
    frame.set_cursor_position((chunks[0].x + 1 + app.input.len() as u16, chunks[0].y + 1));
    let log_items: Vec<ListItem> = app.log.iter().rev().take(20).map(|l| ListItem::new(Line::from(l.as_str()))).collect();
    let log_list = List::new(log_items).block(Block::default().borders(Borders::ALL).title(" Log ").style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(log_list, chunks[1]);
}

fn render_select_app(app: &mut App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = app.search_results.iter()
        .enumerate()
        .map(|(i, (t, p))| ListItem::new(Line::from(format!("{}. {} ({})", i + 1, t, p))))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Select app "))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_select_version(app: &mut App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = app.versions.iter()
        .enumerate()
        .map(|(i, v)| ListItem::new(Line::from(format!("{}. {}", i + 1, v.label))))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Select version "))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_downloading(app: &App, frame: &mut Frame, area: Rect) {
    let chunks = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    let items: Vec<ListItem> = app.log.iter().rev().take(30).map(|l| ListItem::new(Line::from(l.as_str()))).collect();
    let log_list = List::new(items).style(Style::default().fg(Color::Green)).
        block(Block::default().borders(Borders::ALL).title(" Progress "));
    frame.render_widget(log_list, chunks[0]);
    let status = Paragraph::new(format!("Downloading {}...\n\nPress Esc to cancel.", app.selected_pkg))
        .block(Block::default().borders(Borders::ALL).title(" Status "))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, chunks[1]);
}

fn render_done(app: &App, frame: &mut Frame, area: Rect) {
    let (icon, color, msg) = if app.download_ok {
        ("✓", Color::Green, app.download_msg.clone())
    } else {
        ("✗", Color::Red, app.download_msg.clone())
    };
    let text = vec![
        Line::from(Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD))),
        Line::from(msg), Line::from(""), Line::from("Press Enter to search again, q to quit"),
    ];
    let para = Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Result "))
        .wrap(Wrap { trim: true });
    frame.render_widget(para, area);
}

pub fn run_tui(mut app: App) -> Result<(), Box<dyn std::error::Error>> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal::enable_raw_mode()?;
    crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;

    while !app.quit {
        terminal.draw(|f| render(&mut app, f))?;
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press { continue; }
                match key.code {
                    KeyCode::Char('q') => { app.quit = true; }
                    KeyCode::Tab => { app.selected_arch = (app.selected_arch + 1) % ARCHES.len(); }
                    KeyCode::Esc => { app.mode = Mode::Search; app.input.clear(); app.search_results.clear(); }
                    KeyCode::Enter => match app.mode {
                        Mode::Search => { if !app.input.is_empty() { app.do_search(); } }
                        Mode::SelectApp => { app.confirm_app(); }
                        Mode::SelectVersion => { app.start_download(); }
                        Mode::Done => { app.mode = Mode::Search; app.input.clear(); app.log.clear(); }
                        Mode::Downloading => {}
                    },
                    KeyCode::Down => {
                        if let Some(sel) = app.list_state.selected() {
                            let max = app.search_results.len().max(app.versions.len()).max(1) - 1;
                            app.list_state.select(Some((sel + 1).min(max)));
                        }
                    }
                    KeyCode::Up => { if let Some(sel) = app.list_state.selected() { app.list_state.select(Some(sel.saturating_sub(1))); } }
                    KeyCode::Backspace => { if let Mode::Search = app.mode { app.input.pop(); } }
                    KeyCode::Char(c) => { if let Mode::Search = app.mode { app.input.push(c); } }
                    _ => {}
                }
            }
        }
        app.tick();
    }

    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

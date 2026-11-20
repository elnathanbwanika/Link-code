//! The bottom panel: a real PTY terminal, the agent output log and problems.
//!
//! There is no VTE dependency here: a pseudo-terminal is spawned with
//! `portable-pty`, its byte stream is parsed by `vt100`, and the resulting
//! screen is painted into a read-only monospace `TextView`. Keystrokes are
//! translated back into terminal byte sequences and written to the PTY.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::state::UiState;

pub struct BottomPanel {
    pub root: gtk::Box,
    pub notebook: gtk::Notebook,
    pub terminal_output: gtk::TextView,
    pub output_view: gtk::TextView,
    pub problems_box: gtk::Box,
    output_buffer: gtk::TextBuffer,
    terminal: Rc<RefCell<TerminalState>>,
}

impl BottomPanel {
    /// Start a fresh shell in the current workspace, replacing any running one.
    pub fn restart(&self, state: &UiState) {
        let Some(cwd) = state.workspace_path() else {
            self.terminal
                .borrow()
                .view
                .show_message("Open a folder to start a terminal (Ctrl+O).");
            return;
        };
        let configured = state.config.borrow().terminal.shell.clone();
        let shell = if configured.trim().is_empty() {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
        } else {
            configured
        };
        self.terminal.borrow_mut().start(&cwd, &shell);
    }

    /// Ensure a shell is running and type a command into it.
    pub fn run_in_terminal(&self, state: &UiState, command: &str) {
        let running = self.terminal.borrow().session.is_some();
        if !running {
            self.restart(state);
        }
        self.terminal.borrow_mut().send(command.as_bytes());
    }

    pub fn clear_terminal(&self) {
        let terminal = self.terminal.borrow();
        if terminal.session.is_some() {
            drop(terminal);
            // `clear` respects the terminal size and leaves the prompt tidy.
            self.terminal.borrow_mut().send(b"clear\n");
        } else {
            terminal.view.reset(terminal.rows, terminal.cols);
        }
    }

    pub fn focus_terminal(&self) {
        self.terminal_output.grab_focus();
    }
}

pub fn build(state: &Rc<UiState>) -> Rc<BottomPanel> {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-bottom");
    root.set_height_request(160);

    let notebook = gtk::Notebook::new();
    notebook.set_scrollable(true);
    notebook.add_css_class("lc-notebook");
    notebook.add_css_class("lc-bg");
    notebook.set_vexpand(true);

    // ---- terminal -------------------------------------------------------
    let terminal_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    terminal_page.add_css_class("lc-bg");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("lc-terminal-bar");
    bar.set_margin_start(8);
    bar.set_margin_end(6);
    bar.set_margin_top(2);
    bar.set_margin_bottom(1);
    let title = gtk::Label::new(Some("TERMINAL"));
    title.add_css_class("lc-section");
    title.add_css_class("lc-tiny");
    bar.append(&title);
    let shell_label = gtk::Label::new(Some(
        state
            .workspace_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no folder open".to_string())
            .as_str(),
    ));
    shell_label.add_css_class("lc-tiny");
    shell_label.add_css_class("lc-dim");
    shell_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    shell_label.set_hexpand(true);
    shell_label.set_xalign(0.0);
    shell_label.set_margin_start(6);
    bar.append(&shell_label);
    let clear = gtk::Button::with_label("Clear");
    clear.add_css_class("lc-ghost");
    clear.add_css_class("lc-tiny");
    bar.append(&clear);
    let restart = gtk::Button::with_label("Restart");
    restart.add_css_class("lc-ghost");
    restart.add_css_class("lc-tiny");
    bar.append(&restart);
    terminal_page.append(&bar);

    let terminal_buffer = gtk::TextBuffer::new(None);
    let terminal_output = gtk::TextView::with_buffer(&terminal_buffer);
    terminal_output.set_monospace(true);
    terminal_output.set_editable(false);
    terminal_output.set_cursor_visible(false);
    terminal_output.set_can_focus(true);
    terminal_output.set_wrap_mode(gtk::WrapMode::None);
    terminal_output.set_left_margin(10);
    terminal_output.set_right_margin(8);
    terminal_output.set_top_margin(4);
    terminal_output.set_bottom_margin(6);
    terminal_output.add_css_class("lc-code");
    terminal_output.add_css_class("lc-terminal");
    let terminal_scroll = gtk::ScrolledWindow::builder()
        // The PTY wraps at the viewport width, so there is never anything to
        // scroll sideways to; hiding the bar also frees the last rows.
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&terminal_output)
        .build();
    terminal_scroll.add_css_class("lc-bg");
    terminal_page.append(&terminal_scroll);

    let terminal_view = TerminalView::new(
        terminal_output.clone(),
        terminal_buffer.clone(),
        terminal_scroll.clone(),
    );
    let terminal = Rc::new(RefCell::new(TerminalState {
        view: terminal_view,
        session: None,
        incoming: None,
        rows: 24,
        cols: 80,
    }));

    notebook.append_page(&terminal_page, Some(&tab_label("Terminal")));

    // ---- output ---------------------------------------------------------
    let output_buffer = gtk::TextBuffer::new(None);
    let output_view = gtk::TextView::with_buffer(&output_buffer);
    output_view.set_monospace(true);
    output_view.set_editable(false);
    output_view.set_left_margin(10);
    output_view.set_right_margin(10);
    output_view.set_top_margin(6);
    output_view.add_css_class("lc-code");
    output_view.add_css_class("lc-tiny");
    let output_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&output_view)
        .build();
    output_scroll.add_css_class("lc-bg");
    notebook.append_page(&output_scroll, Some(&tab_label("Output")));

    // ---- problems -------------------------------------------------------
    let problems_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    problems_box.set_margin_start(8);
    problems_box.set_margin_end(8);
    problems_box.set_margin_top(6);
    let empty = gtk::Label::new(Some("No problems detected."));
    empty.add_css_class("lc-dim");
    empty.add_css_class("lc-tiny");
    empty.set_xalign(0.0);
    problems_box.append(&empty);
    let problems_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&problems_box)
        .build();
    problems_scroll.add_css_class("lc-bg");
    notebook.append_page(&problems_scroll, Some(&tab_label("Problems")));

    root.append(&notebook);

    let panel = Rc::new(BottomPanel {
        root,
        notebook,
        terminal_output: terminal_output.clone(),
        output_view,
        problems_box,
        output_buffer,
        terminal: terminal.clone(),
    });

    // ---- key forwarding -------------------------------------------------
    {
        let terminal = terminal.clone();
        let view = terminal_output.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, modifiers| {
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);

            // Terminal copy/paste, matching the usual terminal shortcuts.
            if control && shift && keyval == gtk::gdk::Key::C {
                view.emit_copy_clipboard();
                return glib::Propagation::Stop;
            }
            if control && shift && keyval == gtk::gdk::Key::V {
                let clipboard = view.clipboard();
                let terminal = terminal.clone();
                clipboard.read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
                    if let Ok(Some(text)) = result {
                        terminal.borrow_mut().send(text.as_bytes());
                    }
                });
                return glib::Propagation::Stop;
            }

            match key_bytes(keyval, modifiers) {
                Some(bytes) => {
                    terminal.borrow_mut().send(&bytes);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        terminal_output.add_controller(key);
    }

    // Drain the reader thread on the GTK thread. A channel is used because
    // GTK widgets are not `Send`; the reader thread only sends byte chunks.
    {
        let terminal = terminal.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            terminal.borrow_mut().drain();
            glib::ControlFlow::Continue
        });
    }

    // The PTY and the parser must agree on a size. GTK4 has no
    // size-allocate signal, so poll the allocation while the panel is mapped.
    {
        let terminal = terminal.clone();
        glib::timeout_add_local(Duration::from_millis(400), move || {
            let (width, height, context) = {
                let terminal = terminal.borrow();
                (
                    terminal.view.viewport.width(),
                    terminal.view.viewport.height(),
                    terminal.view.view.pango_context(),
                )
            };
            if width > 40 && height > 20 {
                let metrics = context.metrics(None, None);
                let char_width = (metrics.approximate_char_width() as f64
                    / gtk::pango::SCALE as f64)
                    .max(5.0);
                let line_height = ((metrics.ascent() + metrics.descent()) as f64
                    / gtk::pango::SCALE as f64)
                    .max(9.0);
                let cols = (((width as f64 - 26.0) / char_width).floor() as u16).clamp(20, 400);
                // Leave a little slack below the last row so the prompt is
                // never clipped by the panel edge.
                let rows =
                    (((height as f64 - 16.0) / line_height).floor() as u16).clamp(5, 200);
                terminal.borrow_mut().resize(rows, cols);
            }
            glib::ControlFlow::Continue
        });
    }

    // ---- buttons --------------------------------------------------------
    {
        let panel = panel.clone();
        let state = state.clone();
        restart.connect_clicked(move |_| panel.restart(&state));
    }
    {
        let panel = panel.clone();
        clear.connect_clicked(move |_| panel.clear_terminal());
    }

    if state.workspace_path().is_some() {
        panel.restart(state);
    } else {
        terminal
            .borrow()
            .view
            .show_message("Open a folder to start a terminal (Ctrl+O).");
    }
    // Keep the workspace hint honest when the folder changes.
    {
        let shell_label = shell_label.clone();
        let state = state.clone();
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let text = state
                .workspace_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no folder open".to_string());
            if shell_label.text() != text {
                shell_label.set_text(&text);
            }
            glib::ControlFlow::Continue
        });
    }

    panel
}

fn tab_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("lc-section");
    label.add_css_class("lc-tiny");
    label
}

impl BottomPanel {
    /// Append a line to the Output tab.
    pub fn log(&self, line: &str) {
        let mut end = self.output_buffer.end_iter();
        self.output_buffer.insert(&mut end, line);
        self.output_buffer.insert(&mut end, "\n");
        let mut end = self.output_buffer.end_iter();
        self.output_view.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
    }

    pub fn show(&self, tab: &str) {
        for index in 0..self.notebook.n_pages() {
            if let Some(page) = self.notebook.nth_page(Some(index)) {
                if let Some(child) = self.notebook.tab_label(&page) {
                    if let Some(label) = child.downcast_ref::<gtk::Label>() {
                        if label.text() == tab {
                            self.notebook.set_current_page(Some(index));
                            return;
                        }
                    }
                }
            }
        }
    }

    pub fn terminal(&self) -> &gtk::TextView {
        &self.terminal_output
    }

    /// Replace the Problems list.
    pub fn set_problems(&self, problems: &[String]) {
        while let Some(child) = self.problems_box.first_child() {
            self.problems_box.remove(&child);
        }
        if problems.is_empty() {
            let empty = gtk::Label::new(Some("No problems detected."));
            empty.add_css_class("lc-dim");
            empty.add_css_class("lc-tiny");
            empty.set_xalign(0.0);
            self.problems_box.append(&empty);
            return;
        }
        for problem in problems {
            let label = gtk::Label::new(Some(problem));
            label.add_css_class("lc-tiny");
            label.add_css_class("lc-warn");
            label.set_xalign(0.0);
            label.set_wrap(true);
            self.problems_box.append(&label);
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal rendering
// ---------------------------------------------------------------------------

/// Paints a `vt100` screen into a GTK text view.
struct TerminalView {
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    parser: RefCell<vt100::Parser>,
    cursor_tag: gtk::TextTag,
    /// Measured for the PTY size instead of the text view, whose width grows
    /// with its content when wrapping is disabled.
    viewport: gtk::ScrolledWindow,
}

impl TerminalView {
    fn new(
        view: gtk::TextView,
        buffer: gtk::TextBuffer,
        viewport: gtk::ScrolledWindow,
    ) -> Rc<Self> {
        let cursor_tag = buffer
            .create_tag(Some("terminal-cursor"), &[])
            .expect("cursor tag");
        cursor_tag.set_property("background", "#2f6fed");
        cursor_tag.set_property("foreground", "#ffffff");
        Rc::new(Self {
            view,
            buffer,
            parser: RefCell::new(vt100::Parser::new(24, 80, 2000)),
            cursor_tag,
            viewport,
        })
    }

    fn reset(&self, rows: u16, cols: u16) {
        *self.parser.borrow_mut() = vt100::Parser::new(rows, cols, 2000);
        self.buffer.set_text("");
    }

    fn set_size(&self, rows: u16, cols: u16) {
        let mut parser = self.parser.borrow_mut();
        let (current_rows, current_cols) = parser.screen().size();
        if current_rows != rows || current_cols != cols {
            parser.set_size(rows, cols);
        }
    }

    fn feed(&self, bytes: &[u8]) {
        self.parser.borrow_mut().process(bytes);
        self.render();
    }

    fn show_message(&self, message: &str) {
        self.buffer.set_text(message);
    }

    fn render(&self) {
        let parser = self.parser.borrow();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let mut text = String::with_capacity(rows as usize * (cols as usize + 1));
        for row in 0..rows {
            let mut line = String::new();
            for col in 0..cols {
                match screen.cell(row, col) {
                    Some(cell) => {
                        let contents = cell.contents();
                        if contents.is_empty() {
                            line.push(' ');
                        } else {
                            line.push_str(&contents);
                        }
                    }
                    None => line.push(' '),
                }
            }
            // Keep at least one cell under the cursor so it can be highlighted.
            if row == cursor_row {
                while line.chars().count() <= cursor_col as usize {
                    line.push(' ');
                }
            }
            // Terminal rows are padded to the full width; trim the padding but
            // never the cell the cursor sits on.
            let min_chars = if row == cursor_row {
                cursor_col as usize + 1
            } else {
                0
            };
            let mut chars = line.chars().count();
            let mut end = line.len();
            while end > 0 && chars > min_chars && line.as_bytes()[end - 1] == b' ' {
                end -= 1;
                chars -= 1;
            }
            line.truncate(end);
            text.push_str(&line);
            if row + 1 < rows {
                text.push('\n');
            }
        }
        self.buffer.set_text(&text);
        if let Some(mut start) = self.buffer.iter_at_line(cursor_row as i32) {
            start.set_line_offset(cursor_col as i32);
            let mut end = start;
            if end.forward_char() {
                self.buffer.apply_tag(&self.cursor_tag, &start, &end);
            }
            let mut scroll = start;
            self.view
                .scroll_to_iter(&mut scroll, 0.0, false, 0.0, 0.0);
            // Like a terminal, never drift sideways; long lines are clipped
            // (the PTY width is kept in sync with the viewport above).
            if let Some(adjustment) = self.view.hadjustment() {
                adjustment.set_value(0.0);
            }
        }
    }
}

/// Owns the PTY process and its writer.
struct PtySession {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

struct TerminalState {
    view: Rc<TerminalView>,
    session: Option<PtySession>,
    /// Chunks written by the reader thread, drained on the GTK thread.
    incoming: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    rows: u16,
    cols: u16,
}

impl TerminalState {
    fn start(&mut self, cwd: &Path, shell: &str) {
        self.session = None;
        self.incoming = None;
        self.view.reset(self.rows, self.cols);
        let size = portable_pty::PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        match spawn_shell(cwd, shell, size, sender) {
            Ok(session) => {
                self.session = Some(session);
                self.incoming = Some(receiver);
            }
            Err(error) => self
                .view
                .show_message(&format!("Could not start {shell}: {error}\n")),
        }
    }

    /// Feed whatever the shell wrote since the last tick into the parser.
    fn drain(&mut self) {
        let mut chunks = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = self.incoming.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(chunk) => chunks.push(chunk),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for chunk in chunks {
            if !chunk.is_empty() {
                self.view.feed(&chunk);
            }
        }
        if disconnected {
            self.incoming = None;
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Some(session) = self.session.as_mut() {
            let _ = session.writer.write_all(bytes);
            let _ = session.writer.flush();
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.view.set_size(rows, cols);
        if let Some(session) = self.session.as_ref() {
            let _ = session.master.resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        self.view.render();
    }
}

fn spawn_shell(
    cwd: &Path,
    shell: &str,
    size: portable_pty::PtySize,
    sender: std::sync::mpsc::Sender<Vec<u8>>,
) -> Result<PtySession, String> {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system.openpty(size).map_err(|e| e.to_string())?;

    let mut command = portable_pty::CommandBuilder::new(shell);
    command.cwd(cwd);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("LINK_CODE", "1");

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| e.to_string())?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    std::thread::Builder::new()
        .name("link-terminal-reader".to_string())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if sender.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(PtySession {
        master: pair.master,
        writer,
        child,
    })
}

/// Map a key press to the bytes a terminal application expects.
fn key_bytes(
    keyval: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
) -> Option<Vec<u8>> {
    use gtk::gdk::Key;
    let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
    let alt = modifiers.contains(gtk::gdk::ModifierType::ALT_MASK);

    let sequence: &[u8] = match keyval {
        Key::Return | Key::KP_Enter => b"\r",
        Key::BackSpace => b"\x7f",
        Key::Tab => b"\t",
        Key::ISO_Left_Tab => b"\x1b[Z",
        Key::Escape => b"\x1b",
        Key::Up | Key::KP_Up => b"\x1b[A",
        Key::Down | Key::KP_Down => b"\x1b[B",
        Key::Right | Key::KP_Right => b"\x1b[C",
        Key::Left | Key::KP_Left => b"\x1b[D",
        Key::Home | Key::KP_Home => b"\x1b[H",
        Key::End | Key::KP_End => b"\x1b[F",
        Key::Insert | Key::KP_Insert => b"\x1b[2~",
        Key::Delete | Key::KP_Delete => b"\x1b[3~",
        Key::Page_Up | Key::KP_Page_Up => b"\x1b[5~",
        Key::Page_Down | Key::KP_Page_Down => b"\x1b[6~",
        Key::F1 => b"\x1bOP",
        Key::F2 => b"\x1bOQ",
        Key::F3 => b"\x1bOR",
        Key::F4 => b"\x1bOS",
        Key::F5 => b"\x1b[15~",
        Key::F6 => b"\x1b[17~",
        Key::F7 => b"\x1b[18~",
        Key::F8 => b"\x1b[19~",
        Key::F9 => b"\x1b[20~",
        Key::F10 => b"\x1b[21~",
        Key::F11 => b"\x1b[23~",
        Key::F12 => b"\x1b[24~",
        _ => {
            let character = keyval.to_unicode()?;
            if control {
                if character == ' ' {
                    return Some(vec![0]);
                }
                if character == '?' {
                    return Some(vec![0x7f]);
                }
                let upper = character.to_ascii_uppercase() as u32;
                if (b'@' as u32..=b'_' as u32).contains(&upper) {
                    return Some(vec![(upper & 0x1f) as u8]);
                }
                return None;
            }
            let mut bytes = Vec::with_capacity(5);
            if alt {
                bytes.push(0x1b);
            }
            let mut encoded = [0u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            return Some(bytes);
        }
    };

    let mut bytes = Vec::with_capacity(sequence.len() + 1);
    if alt {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(sequence);
    Some(bytes)
}

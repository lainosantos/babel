//! Small software-rendered native HUD, isolated from the audio process.
//!
//! One bounded stdin mailbox, no network/webview/GPU context, no idle redraws.
//! X11 (including XWayland), Cocoa and Win32 provide a non-activating window.
//! Pure Wayland falls back in the parent to compositor-managed notifications.
use anyhow::{Context, Result, bail};
use fontdue::{Font, FontSettings, Metrics};
use serde::{Deserialize, Serialize};
use softbuffer::{Context as SurfaceContext, Surface};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, Write},
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Theme, Window, WindowAttributes, WindowId, WindowLevel},
};

pub const READY: &str = "BABEL_FEEDBACK_READY";
const MAX_LINE: usize = 4096;
const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 104.0;
const ACTIVATION_MINIMUM: Duration = Duration::from_millis(300);
const ANIMATION: Duration = Duration::from_millis(1400);
const IDLE_EXIT: Duration = Duration::from_secs(30);
const FRAME: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackPhase {
    Activated,
    Processing,
    Succeeded,
    Failed,
    Dismissed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackMessage {
    pub activation_id: u64,
    pub phase: FeedbackPhase,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default = "motion_default")]
    pub motion: bool,
}

fn motion_default() -> bool {
    true
}

impl FeedbackMessage {
    fn sanitize(&mut self) {
        self.title = bounded_text(&self.title, 100);
        self.detail = bounded_text(&self.detail, 180);
    }
}

fn bounded_text(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

#[derive(Debug)]
struct Visible {
    message: FeedbackMessage,
    since: Instant,
    expires: Option<Instant>,
}

/// Pure timing state; tested without a display server or audio capture.
struct Timeline {
    visible: Option<Visible>,
    pending: Option<FeedbackMessage>,
    last_activation: u64,
    closed_activation: u64,
    idle_since: Instant,
}

impl Timeline {
    fn new(now: Instant) -> Self {
        Self {
            visible: None,
            pending: None,
            last_activation: 0,
            closed_activation: 0,
            idle_since: now,
        }
    }

    fn accept(&mut self, mut message: FeedbackMessage, now: Instant) {
        message.sanitize();
        if message.activation_id < self.last_activation {
            return;
        }
        if message.activation_id > 0 && message.activation_id <= self.closed_activation {
            return;
        }
        self.last_activation = message.activation_id;
        if message.phase == FeedbackPhase::Dismissed {
            self.hide(now);
            return;
        }
        if message.activation_id == 0 {
            return;
        }
        if let Some(visible) = &self.visible
            && visible.message.activation_id == message.activation_id
        {
            if visible.message.phase == message.phase {
                return;
            }
            // A very fast result must still acknowledge the wake-name activation.
            if visible.message.phase == FeedbackPhase::Activated
                && now < visible.since + ACTIVATION_MINIMUM
            {
                self.pending = Some(message);
                return;
            }
            // Late intermediate messages cannot resurrect a completed command.
            if matches!(
                visible.message.phase,
                FeedbackPhase::Succeeded | FeedbackPhase::Failed
            ) {
                return;
            }
        }
        self.show(message, now);
    }

    fn show(&mut self, message: FeedbackMessage, now: Instant) {
        let expires = match message.phase {
            FeedbackPhase::Succeeded => Some(now + Duration::from_secs(5)),
            FeedbackPhase::Failed => Some(now + Duration::from_secs(9)),
            // The supervisor supplies command cancellation/timeouts. Model load
            // and multi-tool commands can legitimately take longer than 30 s.
            _ => None,
        };
        self.pending = None;
        self.visible = Some(Visible {
            message,
            since: now,
            expires,
        });
    }

    fn hide(&mut self, now: Instant) {
        if self.visible.take().is_some() {
            self.idle_since = now;
        }
        self.closed_activation = self.last_activation;
        self.pending = None;
    }

    fn advance(&mut self, now: Instant) -> bool {
        if self.pending.is_some()
            && self
                .visible
                .as_ref()
                .is_some_and(|visible| now >= visible.since + ACTIVATION_MINIMUM)
        {
            let message = self.pending.take().expect("pending message checked");
            self.show(message, now);
            return true;
        }
        if self
            .visible
            .as_ref()
            .is_some_and(|visible| visible.expires.is_some_and(|expires| now >= expires))
        {
            self.hide(now);
            return true;
        }
        false
    }

    fn deadline(&self, now: Instant, motion: bool) -> Option<Instant> {
        let Some(visible) = &self.visible else {
            return Some(self.idle_since + IDLE_EXIT);
        };
        let mut deadline = visible.expires;
        if self.pending.is_some() {
            let pending = visible.since + ACTIVATION_MINIMUM;
            deadline = Some(deadline.map_or(pending, |deadline| deadline.min(pending)));
        }
        if animate(visible, now, motion) {
            let frame = now + FRAME;
            deadline = Some(deadline.map_or(frame, |deadline| deadline.min(frame)));
        }
        deadline
    }
}

fn animate(visible: &Visible, now: Instant, system_motion: bool) -> bool {
    system_motion
        && visible.message.motion
        && matches!(
            visible.message.phase,
            FeedbackPhase::Activated | FeedbackPhase::Processing
        )
        && now < visible.since + ANIMATION
}

#[derive(Debug)]
enum InputEvent {
    Messages,
    Closed,
}

type Inbox = Arc<Mutex<VecDeque<FeedbackMessage>>>;

pub fn run() -> Result<()> {
    let mut builder = EventLoop::<InputEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::EventLoopBuilderExtX11;
        // Ordinary Wayland toplevels cannot promise no activation/positioning.
        // Prefer the compositor's system notification fallback to stealing focus.
        if std::env::var_os("DISPLAY").is_none_or(|display| display.is_empty()) {
            bail!("Native feedback needs X11 or XWayland");
        }
        builder.with_x11();
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false)
            .with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder.build()?;
    let inbox: Inbox = Arc::new(Mutex::new(VecDeque::with_capacity(32)));
    let input = inbox.clone();
    let proxy = event_loop.create_proxy();
    std::thread::Builder::new()
        .name("babel-feedback-input".into())
        .spawn(move || {
            let mut reader = std::io::stdin().lock();
            while let Ok(Some(message)) = read_message(&mut reader) {
                let wake = {
                    let mut input = input.lock().unwrap_or_else(|error| error.into_inner());
                    let wake = input.is_empty();
                    if input.len() == 32 {
                        input.pop_front();
                    }
                    input.push_back(message);
                    wake
                };
                if wake && proxy.send_event(InputEvent::Messages).is_err() {
                    return;
                }
            }
            let _ = proxy.send_event(InputEvent::Closed);
        })?;
    let mut app = Hud::new(inbox)?;
    event_loop.run_app(&mut app)?;
    if app.failed {
        bail!("Native feedback surface unavailable");
    }
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<FeedbackMessage>> {
    let mut line = Vec::with_capacity(512);
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(std::io::ErrorKind::UnexpectedEof.into())
            };
        }
        let end = chunk.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(chunk.len(), |end| end + 1);
        if line.len() + count > MAX_LINE {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        line.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if end.is_some() {
            return serde_json::from_slice(&line)
                .map(Some)
                .map_err(|_| std::io::ErrorKind::InvalidData.into());
        }
    }
}

struct Hud {
    window: Option<Arc<Window>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    painter: Painter,
    timeline: Timeline,
    inbox: Inbox,
    system_motion: bool,
    dark: bool,
    shown: bool,
    failed: bool,
    last_frame: Instant,
}

impl Hud {
    fn new(inbox: Inbox) -> Result<Self> {
        Ok(Self {
            window: None,
            surface: None,
            painter: Painter::new()?,
            timeline: Timeline::new(Instant::now()),
            inbox,
            system_motion: system_allows_motion(),
            dark: false,
            shown: false,
            failed: false,
            last_frame: Instant::now(),
        })
    }

    fn create(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        self.create_surface(event_loop, false)?;
        // Allocate and paint before reporting readiness; an unsupported display
        // or failed framebuffer must make the parent choose its fallback.
        self.draw()?;
        writeln!(std::io::stdout(), "{READY}")?;
        std::io::stdout().flush()?;
        Ok(())
    }

    fn create_surface(&mut self, event_loop: &ActiveEventLoop, visible: bool) -> Result<()> {
        let attributes = Window::default_attributes()
            .with_title("Babel")
            .with_inner_size(LogicalSize::new(WIDTH, HEIGHT))
            .with_resizable(false)
            .with_decorations(false)
            .with_visible(visible)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = Arc::new(event_loop.create_window(platform_attributes(attributes))?);
        // This is a passive visual indicator: it must not intercept typing/clicks.
        window.set_cursor_hittest(false)?;
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{CornerPreference, WindowExtWindows};
            window.set_enable(false);
            window.set_corner_preference(CornerPreference::Round);
        }
        self.dark = window.theme() == Some(Theme::Dark);
        let context = SurfaceContext::new(window.clone())
            .map_err(|_| anyhow::anyhow!("Feedback display context unavailable"))?;
        let surface = Surface::new(&context, window.clone())
            .map_err(|_| anyhow::anyhow!("Feedback surface unavailable"))?;
        self.surface = Some(surface);
        self.window = Some(window);
        self.position(event_loop);
        Ok(())
    }

    fn position(&self, event_loop: &ActiveEventLoop) {
        let Some(window) = &self.window else { return };
        if let Some(monitor) = event_loop
            .primary_monitor()
            .or_else(|| window.current_monitor())
        {
            let size = monitor.size();
            let origin = monitor.position();
            let scale = monitor.scale_factor();
            // The top margin leaves common desktop menu/panel areas unobscured.
            let width = (WIDTH * scale).round() as i32;
            let margin = (24.0 * scale).round() as i32;
            window.set_outer_position(PhysicalPosition::new(
                origin.x + (size.width as i32 - width - margin).max(0),
                origin.y + (64.0 * scale).round() as i32,
            ));
        }
    }

    fn sync_window(&mut self, event_loop: &ActiveEventLoop) {
        let visible = self.timeline.visible.is_some();
        if self.shown != visible {
            self.shown = visible;
            #[cfg(target_os = "macos")]
            if visible {
                // Winit 0.30's set_visible(true) calls makeKeyAndOrderFront on
                // Cocoa, ignoring the initial with_active(false). Recreating a
                // visible inactive window uses orderFront and never takes focus.
                if self.create_surface(event_loop, true).is_err() {
                    self.fail(event_loop);
                    return;
                }
            }
            if visible {
                self.position(event_loop);
            }
            if let Some(window) = &self.window {
                #[cfg(not(target_os = "macos"))]
                window.set_visible(visible);
                #[cfg(target_os = "macos")]
                if !visible {
                    window.set_visible(false);
                }
            }
        }
        if let Some(window) = &self.window
            && let Some(visible) = &self.timeline.visible
        {
            window.set_title(&format!("Babel · {}", visible.message.title));
            window.request_redraw();
        }
    }

    fn draw(&mut self) -> Result<()> {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        // Defend memory use against nonsensical resize messages/compositor scale.
        if size.width > 4096 || size.height > 2048 {
            bail!("Feedback surface too large");
        }
        surface
            .resize(width, height)
            .map_err(|_| anyhow::anyhow!("Feedback resize failed"))?;
        let now = Instant::now();
        let pixmap = self.painter.paint(
            size.width,
            size.height,
            self.timeline.visible.as_ref(),
            now,
            self.dark,
            self.system_motion,
        )?;
        let mut buffer = surface
            .buffer_mut()
            .map_err(|_| anyhow::anyhow!("Feedback buffer unavailable"))?;
        for (destination, source) in buffer.iter_mut().zip(pixmap.data().as_chunks::<4>().0) {
            *destination =
                (u32::from(source[0]) << 16) | (u32::from(source[1]) << 8) | u32::from(source[2]);
        }
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|_| anyhow::anyhow!("Feedback presentation failed"))?;
        self.last_frame = now;
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop) {
        self.failed = true;
        event_loop.exit();
    }
}

impl ApplicationHandler<InputEvent> for Hud {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() && self.create(event_loop).is_err() {
            self.fail(event_loop);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: InputEvent) {
        match event {
            InputEvent::Closed => event_loop.exit(),
            InputEvent::Messages => {
                let mut messages = VecDeque::new();
                std::mem::swap(
                    &mut *self.inbox.lock().unwrap_or_else(|error| error.into_inner()),
                    &mut messages,
                );
                for message in messages {
                    self.timeline.accept(message, Instant::now());
                }
                self.sync_window(event_loop);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                if self.draw().is_err() {
                    self.fail(event_loop);
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.position(event_loop);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                self.dark = theme == Theme::Dark;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                self.timeline.hide(Instant::now());
                self.sync_window(event_loop);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let changed = self.timeline.advance(now);
        if self.timeline.visible.is_none() && now >= self.timeline.idle_since + IDLE_EXIT {
            event_loop.exit();
            return;
        }
        if changed {
            self.sync_window(event_loop);
        }
        let animated = self
            .timeline
            .visible
            .as_ref()
            .is_some_and(|visible| animate(visible, now, self.system_motion));
        if animated
            && now >= self.last_frame + FRAME
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        let mut deadline = self.timeline.deadline(now, self.system_motion);
        if animated {
            let next_frame = self.last_frame + FRAME;
            deadline = Some(deadline.map_or(next_frame, |deadline| deadline.min(next_frame)));
        }
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

fn platform_attributes(attributes: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::{WindowAttributesExtX11, WindowType};
        attributes
            .with_name("babel-feedback", "Babel")
            .with_override_redirect(true)
            .with_x11_window_type(vec![WindowType::Notification])
    }
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        attributes
            .with_skip_taskbar(true)
            .with_undecorated_shadow(true)
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attributes
            .with_has_shadow(true)
            .with_accepts_first_mouse(false)
    }
}

fn system_allows_motion() -> bool {
    if std::env::var_os("BABEL_REDUCED_MOTION").is_some_and(|value| value == "1") {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        windows::UI::ViewManagement::UISettings::new()
            .and_then(|settings| settings.AnimationsEnabled())
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        !objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
    #[cfg(target_os = "linux")]
    {
        // GNOME exposes the preference without introducing a GTK runtime. Other
        // desktops conservatively get the static version when it is unknown.
        use std::{
            io::Read,
            process::{Command, Stdio},
        };
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if !desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("gnome") || name.eq_ignore_ascii_case("unity"))
        {
            return false;
        }
        let Ok(mut child) = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "enable-animations"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let mut output = String::new();
                    return child.stdout.take().is_some_and(|stdout| {
                        stdout.take(64).read_to_string(&mut output).is_ok()
                            && output.trim() == "true"
                    });
                }
                Ok(Some(_)) | Err(_) => return false,
                _ if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

struct Painter {
    font: Font,
    glyphs: BTreeMap<(char, u16), (Metrics, Vec<u8>)>,
}

impl Painter {
    fn new() -> Result<Self> {
        Ok(Self {
            font: Font::from_bytes(
                include_bytes!("../../assets/fonts/manrope-medium.ttf").as_slice(),
                FontSettings::default(),
            )
            .map_err(|_| anyhow::anyhow!("Bundled feedback font is invalid"))?,
            glyphs: BTreeMap::new(),
        })
    }

    fn paint(
        &mut self,
        width: u32,
        height: u32,
        visible: Option<&Visible>,
        now: Instant,
        dark: bool,
        motion: bool,
    ) -> Result<Pixmap> {
        let mut pixmap =
            Pixmap::new(width, height).context("Feedback framebuffer allocation failed")?;
        let background = if dark { [25, 42, 66] } else { [255, 255, 255] };
        let ink = if dark { [246, 248, 252] } else { [25, 42, 66] };
        let secondary = if dark {
            [180, 195, 215]
        } else {
            [102, 117, 138]
        };
        pixmap.fill(rgb(background));
        let Some(visible) = visible else {
            return Ok(pixmap);
        };
        let scale = width as f32 / WIDTH as f32;
        let accent = match visible.message.phase {
            FeedbackPhase::Succeeded => [20, 125, 128],
            FeedbackPhase::Failed => [179, 94, 85],
            _ => [52, 92, 244],
        };
        let transform = Transform::from_scale(scale, scale);
        let elapsed = now.saturating_duration_since(visible.since).as_secs_f32();
        let active_motion = animate(visible, now, motion);
        let mut paint = Paint::default();
        paint.set_color(rgb(if dark { [52, 71, 97] } else { [225, 232, 241] }));
        if let Some(border) =
            tiny_skia::Rect::from_xywh(0.5, 0.5, WIDTH as f32 - 1.0, HEIGHT as f32 - 1.0)
        {
            let mut path = PathBuilder::new();
            path.push_rect(border);
            pixmap.stroke_path(
                &path.finish().expect("rectangle path"),
                &paint,
                &Stroke {
                    width: 1.0,
                    ..Stroke::default()
                },
                transform,
                None,
            );
        }
        paint.set_color(rgb(accent));
        pixmap.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, 3.0, HEIGHT as f32).expect("accent rectangle"),
            &paint,
            transform,
            None,
        );
        let mut logo = PathBuilder::new();
        logo.push_circle(43.0, 51.0, 22.0);
        paint.set_color(Color::from_rgba8(
            accent[0],
            accent[1],
            accent[2],
            if dark { 48 } else { 18 },
        ));
        pixmap.fill_path(
            &logo.finish().expect("logo background"),
            &paint,
            FillRule::Winding,
            transform,
            None,
        );
        if active_motion {
            let progress = (elapsed / ANIMATION.as_secs_f32()).clamp(0.0, 1.0);
            let radius = 24.0 + 3.0 * (progress * std::f32::consts::PI).sin();
            let mut halo = PathBuilder::new();
            halo.push_circle(43.0, 51.0, radius);
            paint.set_color(Color::from_rgba8(
                accent[0],
                accent[1],
                accent[2],
                (48.0 * (1.0 - progress)) as u8,
            ));
            pixmap.stroke_path(
                &halo.finish().expect("halo"),
                &paint,
                &Stroke {
                    width: 1.5,
                    ..Stroke::default()
                },
                transform,
                None,
            );
            if visible.message.phase == FeedbackPhase::Processing {
                let angle = progress * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                let mut dot = PathBuilder::new();
                dot.push_circle(
                    43.0 + angle.cos() * radius,
                    51.0 + angle.sin() * radius,
                    2.0,
                );
                paint.set_color(rgb(accent));
                pixmap.fill_path(
                    &dot.finish().expect("orbit dot"),
                    &paint,
                    FillRule::Winding,
                    transform,
                    None,
                );
            }
        }
        // The same B/stroke geometry as assets/babel.svg, with a small state mark.
        let mut mark = PathBuilder::new();
        mark.move_to(35.0, 40.0);
        mark.line_to(35.0, 62.0);
        mark.move_to(35.0, 40.0);
        mark.line_to(44.6, 40.0);
        mark.cubic_to(51.9, 40.0, 51.9, 51.0, 44.6, 51.0);
        mark.line_to(35.0, 51.0);
        mark.line_to(47.4, 51.0);
        mark.cubic_to(54.7, 51.0, 54.7, 62.0, 47.4, 62.0);
        mark.line_to(35.0, 62.0);
        mark.move_to(55.6, 41.4);
        mark.line_to(55.6, 45.5);
        paint.set_color(rgb(accent));
        pixmap.stroke_path(
            &mark.finish().expect("Babel mark"),
            &paint,
            &Stroke {
                width: 2.2,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            },
            transform,
            None,
        );
        self.text(
            &mut pixmap,
            "Babel",
            80.0,
            27.0,
            11.0,
            secondary,
            scale,
            260.0,
        );
        self.text(
            &mut pixmap,
            &visible.message.title,
            80.0,
            51.0,
            16.0,
            ink,
            scale,
            264.0,
        );
        self.text(
            &mut pixmap,
            &visible.message.detail,
            80.0,
            75.0,
            12.0,
            secondary,
            scale,
            278.0,
        );
        match visible.message.phase {
            FeedbackPhase::Succeeded | FeedbackPhase::Failed => {
                let mut state = PathBuilder::new();
                if visible.message.phase == FeedbackPhase::Succeeded {
                    state.move_to(346.0, 21.0);
                    state.line_to(350.0, 25.0);
                    state.line_to(357.0, 17.0);
                } else {
                    state.move_to(348.0, 21.0);
                    state.line_to(356.0, 21.0);
                }
                paint.set_color(rgb(accent));
                pixmap.stroke_path(
                    &state.finish().expect("state mark"),
                    &paint,
                    &Stroke {
                        width: 1.8,
                        line_cap: tiny_skia::LineCap::Round,
                        ..Stroke::default()
                    },
                    transform,
                    None,
                );
            }
            _ => {}
        }
        Ok(pixmap)
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        baseline: f32,
        size: f32,
        color: [u8; 3],
        scale: f32,
        max_width: f32,
    ) {
        let px = (size * scale).round().clamp(1.0, 256.0) as u16;
        let mut cursor = x * scale;
        let right = (x + max_width) * scale;
        let baseline = (baseline * scale).round() as i32;
        // Evict across unusual repeated display-scale changes; cached glyphs
        // remain bounded even when the helper is kept busy for a long time.
        if self.glyphs.len() > 1024 {
            self.glyphs.clear();
        }
        let ellipsis_width = self.font.metrics('…', px as f32).advance_width;
        let characters: Vec<char> = text.chars().collect();
        for (index, mut character) in characters.iter().copied().enumerate() {
            let advance = self.font.metrics(character, px as f32).advance_width;
            let truncate =
                index + 1 < characters.len() && cursor + advance + ellipsis_width > right;
            if truncate {
                character = '…';
            }
            let (metrics, bitmap) = self
                .glyphs
                .entry((character, px))
                .or_insert_with(|| self.font.rasterize(character, px as f32));
            let origin_x = cursor.round() as i32 + metrics.xmin;
            let origin_y = baseline - metrics.ymin - metrics.height as i32;
            let width = pixmap.width() as usize;
            let height = pixmap.height() as i32;
            let bytes = pixmap.data_mut();
            for row in 0..metrics.height {
                let y = origin_y + row as i32;
                if y < 0 || y >= height {
                    continue;
                }
                for column in 0..metrics.width {
                    let x = origin_x + column as i32;
                    if x < 0 || x as usize >= width {
                        continue;
                    }
                    let coverage = u32::from(bitmap[row * metrics.width + column]);
                    let index = (y as usize * width + x as usize) * 4;
                    for channel in 0..3 {
                        bytes[index + channel] = ((u32::from(color[channel]) * coverage
                            + u32::from(bytes[index + channel]) * (255 - coverage))
                            / 255) as u8;
                    }
                }
            }
            cursor += metrics.advance_width;
            if truncate || cursor >= right {
                break;
            }
        }
    }
}

fn rgb(color: [u8; 3]) -> Color {
    Color::from_rgba8(color[0], color[1], color[2], 255)
}

/// Developer-only snapshots use exactly the window painter, without any desktop
/// connection. PPM keeps the runtime free of an otherwise unnecessary encoder.
pub fn render_previews(directory: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    let mut painter = Painter::new()?;
    let now = Instant::now();
    for (name, phase, key) in [
        ("activated", FeedbackPhase::Activated, "agent.activated"),
        ("processing", FeedbackPhase::Processing, "agent.processing"),
        ("succeeded", FeedbackPhase::Succeeded, "agent.succeeded"),
        ("failed", FeedbackPhase::Failed, "agent.failed"),
    ] {
        let mut state = Timeline::new(now);
        state.accept(
            FeedbackMessage {
                activation_id: 1,
                phase,
                title: crate::i18n::text("pt", key),
                detail: crate::i18n::text("pt", &format!("{key}_hint")),
                motion: true,
            },
            now,
        );
        for (theme, dark) in [("light", false), ("dark", true)] {
            let pixels = painter.paint(
                760,
                208,
                state.visible.as_ref(),
                now + Duration::from_millis(350),
                dark,
                true,
            )?;
            let path = directory.join(format!("{name}-{theme}.ppm"));
            let mut output = std::io::BufWriter::new(std::fs::File::create(path)?);
            writeln!(output, "P6\n{} {}\n255", pixels.width(), pixels.height())?;
            for pixel in pixels.data().as_chunks::<4>().0 {
                output.write_all(&pixel[..3])?;
            }
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: u64, phase: FeedbackPhase) -> FeedbackMessage {
        FeedbackMessage {
            activation_id: id,
            phase,
            title: "Comando reconhecido".into(),
            detail: "Babel está ouvindo".into(),
            motion: true,
        }
    }

    #[test]
    fn fast_result_preserves_activation_then_expires_and_releases_process() {
        let now = Instant::now();
        let mut state = Timeline::new(now);
        state.accept(message(1, FeedbackPhase::Activated), now);
        state.accept(
            message(1, FeedbackPhase::Processing),
            now + Duration::from_millis(2),
        );
        state.accept(
            message(1, FeedbackPhase::Succeeded),
            now + Duration::from_millis(5),
        );
        assert_eq!(
            state.visible.as_ref().unwrap().message.phase,
            FeedbackPhase::Activated
        );
        assert!(!state.advance(now + Duration::from_millis(299)));
        assert!(state.advance(now + ACTIVATION_MINIMUM));
        assert_eq!(
            state.visible.as_ref().unwrap().message.phase,
            FeedbackPhase::Succeeded
        );
        let expiry = now + ACTIVATION_MINIMUM + Duration::from_secs(5);
        assert!(state.advance(expiry));
        assert!(state.visible.is_none());
        assert_eq!(state.deadline(expiry, true), Some(expiry + IDLE_EXIT));
    }

    #[test]
    fn newer_commands_and_dismissal_clear_queued_stale_results() {
        let now = Instant::now();
        let mut state = Timeline::new(now);
        state.accept(message(1, FeedbackPhase::Activated), now);
        state.accept(message(1, FeedbackPhase::Failed), now);
        state.accept(message(2, FeedbackPhase::Activated), now);
        assert!(state.pending.is_none());
        state.accept(message(1, FeedbackPhase::Succeeded), now);
        assert_eq!(state.visible.as_ref().unwrap().message.activation_id, 2);
        state.accept(message(2, FeedbackPhase::Dismissed), now);
        assert!(state.visible.is_none());
        assert!(state.pending.is_none());
    }

    #[test]
    fn motion_and_visibility_are_bounded_with_no_idle_animation() {
        let now = Instant::now();
        let mut state = Timeline::new(now);
        state.accept(message(1, FeedbackPhase::Processing), now);
        assert_eq!(state.deadline(now, true), Some(now + FRAME));
        assert_eq!(state.deadline(now, false), None);
        assert_eq!(state.deadline(now + ANIMATION, true), None);
        state.visible.as_mut().unwrap().message.motion = false;
        assert_eq!(state.deadline(now, true), None);
    }

    #[test]
    fn protocol_is_bounded_and_accepts_split_utf8_json() {
        let mut bytes = serde_json::to_vec(&message(1, FeedbackPhase::Activated)).unwrap();
        bytes.push(b'\n');
        let mut reader = std::io::BufReader::with_capacity(2, bytes.as_slice());
        assert_eq!(
            read_message(&mut reader).unwrap().unwrap().title,
            "Comando reconhecido"
        );
        assert!(read_message(&mut reader).unwrap().is_none());
        let oversized = vec![b'x'; MAX_LINE + 1];
        assert!(read_message(&mut oversized.as_slice()).is_err());
        let private_unknown = b"{\"speech\":\"private\"}\n";
        assert!(read_message(&mut private_unknown.as_slice()).is_err());
    }

    #[test]
    fn drawing_supports_portuguese_dark_mode_and_scaled_displays() {
        let now = Instant::now();
        let mut state = Timeline::new(now);
        state.accept(message(1, FeedbackPhase::Failed), now);
        let mut painter = Painter::new().unwrap();
        for scale in [1, 2, 3] {
            for dark in [false, true] {
                let pixels = painter
                    .paint(
                        380 * scale,
                        104 * scale,
                        state.visible.as_ref(),
                        now,
                        dark,
                        false,
                    )
                    .unwrap();
                assert_eq!(
                    pixels.data().len(),
                    380 * 104 * scale as usize * scale as usize * 4
                );
                assert!(
                    pixels
                        .data()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|pixel| pixel[3] == 255)
                );
            }
        }
    }
}

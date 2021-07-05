use std::time::Instant;
//use std::time::{Duration, Instant};

use anyhow::{Context as AnyhowContext, Result};
use glium::glutin::dpi::LogicalSize;
use glium::glutin::event::{Event, WindowEvent};
use glium::glutin::event_loop::{ControlFlow, EventLoop};
use glium::glutin::window::WindowBuilder;
use glium::glutin::ContextBuilder;
use glium::{Display, Surface};
use imgui::{Context, Ui};
use imgui_glium_renderer::Renderer;
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use winit::platform::run_return::EventLoopExtRunReturn;

//const WAIT_TIME: Duration = Duration::from_secs(1);

pub trait WindowHandler {
    fn on_draw(&mut self, ui: &mut Ui) -> bool;
    fn on_exit(&mut self);
}

pub struct ImguiWindow {
    event_loop: EventLoop<()>,
    display: Display,
    imgui: Context,
    platform: WinitPlatform,
    renderer: Renderer,
}

impl ImguiWindow {
    pub fn new(title: &str) -> Result<Self> {
        let event_loop = EventLoop::new();
        let context = ContextBuilder::new().with_vsync(true);
        let builder = WindowBuilder::new()
            .with_title(String::from(title))
            .with_inner_size(LogicalSize::new(1024f64, 768f64));
        let display = Display::new(builder, context, &event_loop)
            .context("Failed to create glium display")?;

        let mut imgui = Context::create();
        imgui.set_ini_filename(None);

        let mut platform = WinitPlatform::init(&mut imgui);
        {
            let gl_window = display.gl_window();
            let window = gl_window.window();
            platform.attach_window(imgui.io_mut(), window, HiDpiMode::Rounded);
        }

        let renderer =
            Renderer::init(&mut imgui, &display).context("Failed to initialize renderer")?;

        Ok(Self {
            event_loop,
            display,
            imgui,
            platform,
            renderer,
        })
    }

    pub fn run(self, mut handler: impl WindowHandler + 'static) {
        let ImguiWindow {
            mut event_loop,
            display,
            mut imgui,
            mut platform,
            mut renderer,
        } = self;

        let mut last_frame = Instant::now();

        event_loop.run_return(move |event, _, control_flow| match event {
            Event::NewEvents(_) => {
                imgui.io_mut().update_delta_time(last_frame.elapsed());

                last_frame = Instant::now();
            }
            Event::MainEventsCleared => {
                let gl_window = display.gl_window();
                let window = gl_window.window();

                platform
                    .prepare_frame(imgui.io_mut(), window)
                    .expect("Failed to prepare frame");

                window.request_redraw();
            }
            Event::RedrawRequested(_) => {
                let mut ui = imgui.frame();
                if !handler.on_draw(&mut ui) {
                    handler.on_exit();

                    *control_flow = ControlFlow::Exit;
                }

                let gl_window = display.gl_window();
                let mut target = display.draw();
                target.clear_color_srgb(0.5, 0.5, 0.5, 1.0);
                platform.prepare_render(&ui, gl_window.window());
                renderer
                    .render(&mut target, ui.render())
                    .expect("Failed to render");
                target.finish().expect("Failed to swap buffers");
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                handler.on_exit();

                *control_flow = ControlFlow::Exit;
            }
            event => {
                let gl_window = display.gl_window();
                platform.handle_event(imgui.io_mut(), gl_window.window(), &event);
            }
        });
    }
}

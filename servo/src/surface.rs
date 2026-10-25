// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Offscreen Servo hosting through Day's native canvas and image decoder.
//! Frames are copied through CPU memory; this is an experimental portability path.
use crate::engine::{self, State, Waker};
use day_core::{Bitmap, BuildCx, RNode, rnode_to_id, with_tree};
use day_piece_webview::{WebPatch, WebProps, WebRenderer, renderer};
use day_spec::{DrawOp, Event, GestureKind, KeyEvent, Point, Rect, Size, Support};
use dpi::PhysicalSize;
use image::{
    ImageEncoder,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use servo::{EventLoopWaker, InputEvent};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};

struct View {
    state: Rc<State>,
    node: Cell<Option<RNode>>,
    web: RefCell<Option<servo::WebView>>,
    size: Cell<Size>,
    bitmap: RefCell<Option<Bitmap>>,
    decoding: Cell<bool>,
    pending: RefCell<Vec<WebPatch>>,
    drag: Cell<Point>,
}
thread_local! {
    static VIEWS: RefCell<HashMap<RNode, Rc<View>>> = RefCell::new(HashMap::new());
    static SESSIONS: RefCell<HashMap<u64, Rc<View>>> = RefCell::new(HashMap::new());
}

pub(crate) fn register() -> Result<(), renderer::RegistrationError> {
    let supported = match day_core::toolkit_key() {
        Some("gtk") => cfg!(feature = "gtk"),
        Some("qt") => cfg!(feature = "qt"),
        Some("xaml" | "winui") => cfg!(feature = "xaml"),
        Some("material") => cfg!(feature = "material"),
        Some("arkui") => cfg!(feature = "arkui"),
        _ => false,
    };
    if !supported {
        return Err(renderer::RegistrationError::Unsupported);
    }
    static HOST: renderer::surface::Host = renderer::surface::Host {
        capabilities: renderer::Capabilities {
            private_browsing: Support::Unsupported,
            profiles: Support::Unsupported,
            clear_data: Support::Unsupported,
        },
        attach,
        update,
        release,
    };
    // Once per process, whatever the registration below answers: a capture must not race the
    // frames this host presents (see `frames_settled`).
    static HOLD: std::sync::Once = std::sync::Once::new();
    HOLD.call_once(|| day_spec::capture::register_hold(frames_settled));
    renderer::surface::register(WebRenderer::Servo, &HOST)
}

/// The screenshot hold this host registers (`day_spec::capture::register_hold`): a frame
/// reaches the canvas only after Servo's paint, a read-back, a PNG round trip through the
/// toolkit's decoder, and a replay, none of which the toolkit's render checkpoint can see. A
/// dayscript capture therefore waits until every attached view with a live page has presented
/// at least one frame and has none waiting to be read (`dirty`) or decoding. On an emulator
/// the first frame after a renderer switch can trail the page's JavaScript replies by more
/// than the capture's own latency, which showed as an empty canvas in the proof captures.
fn frames_settled() -> bool {
    VIEWS.with(|views| {
        views.borrow().values().all(|view| {
            view.node.get().is_none()
                || view.web.borrow().is_none()
                || (view.bitmap.borrow().is_some()
                    && !view.state.dirty.get()
                    && !view.decoding.get())
        })
    })
}

fn get(node: RNode) -> Option<Rc<View>> {
    VIEWS.with(|views| views.borrow().get(&node).cloned())
}
fn attach(cx: &mut BuildCx, node: RNode, props: &WebProps) {
    let retained = (props.session != 0)
        .then(|| SESSIONS.with(|s| s.borrow().get(&props.session).cloned()))
        .flatten();
    let view = retained.unwrap_or_else(|| {
        Rc::new(View {
            state: engine::state(props, rnode_to_id(node)),
            node: Cell::new(Some(node)),
            web: RefCell::new(None),
            size: Cell::new(Size::new(0.0, 0.0)),
            bitmap: RefCell::new(None),
            decoding: Cell::new(false),
            pending: RefCell::new(Vec::new()),
            drag: Cell::new(Point::ZERO),
        })
    });
    view.node.set(Some(node));
    view.state.node.set(rnode_to_id(node));
    VIEWS.with(|views| views.borrow_mut().insert(node, view.clone()));
    if props.session != 0 {
        SESSIONS.with(|s| s.borrow_mut().insert(props.session, view.clone()));
    }
    with_tree(|tree| {
        tree.set_focusable(node, true);
        tree.enable_gesture(node, GestureKind::Tap);
        tree.enable_gesture(node, GestureKind::Drag);
    });
    day_spec::keys::mark(rnode_to_id(node));
    cx.on(node, move |event| {
        let Some(view) = get(node) else {
            return;
        };
        match event {
            Event::FrameChanged(size) => view.resize(*size),
            Event::FocusChanged(focused) => view.with_web(|web| {
                if *focused {
                    web.focus();
                } else {
                    web.blur();
                }
            }),
            Event::Tap(point) => {
                with_tree(|tree| tree.focus_node(node, true));
                view.with_web(|web| {
                    let point = servo::DevicePoint::new(point.x as f32, point.y as f32).into();
                    web.notify_input_event(InputEvent::MouseMove(servo::MouseMoveEvent::new(
                        point,
                    )));
                    for action in [servo::MouseButtonAction::Down, servo::MouseButtonAction::Up] {
                        web.notify_input_event(InputEvent::MouseButton(
                            servo::MouseButtonEvent::new(
                                action,
                                servo::MouseButton::Primary,
                                point,
                            ),
                        ));
                    }
                });
            }
            Event::Drag {
                phase,
                location,
                translation,
            } => {
                if *phase == day_spec::DragPhase::Began {
                    view.drag.set(Point::ZERO);
                }
                let previous = view.drag.replace(*translation);
                view.with_web(|web| {
                    web.notify_input_event(InputEvent::Wheel(servo::WheelEvent::new(
                        servo::WheelDelta {
                            x: previous.x - translation.x,
                            y: previous.y - translation.y,
                            z: 0.0,
                            mode: servo::WheelMode::DeltaPixel,
                        },
                        servo::DevicePoint::new(location.x as f32, location.y as f32).into(),
                    )));
                });
            }
            Event::Key(key) => view.key(key),
            _ => {}
        }
    });
    // Layout runs after build. Initialize from the resulting frame; later FrameChanged
    // events resize the context without polling a clock while the application is idle.
    day_reactive::on_main(move || {
        if let Some(view) = get(node)
            && let Some(frame) = with_tree(|tree| tree.node_frame(node))
        {
            view.resize(frame.size);
            view.replay();
        }
    });
}
impl View {
    fn resize(&self, size: Size) {
        #[cfg(all(
            any(feature = "gtk", feature = "qt"),
            target_os = "linux",
            not(target_env = "ohos")
        ))]
        let _graphics = match crate::linux_gl::Guard::enter() {
            Ok(guard) => guard,
            Err(error) => {
                log::error!("Servo graphics binding: {error}");
                return;
            }
        };
        if !size.width.is_finite()
            || !size.height.is_finite()
            || size.width <= 0.0
            || size.height <= 0.0
        {
            return;
        }
        self.size.set(size);
        let pixels = PhysicalSize::new(
            size.width.ceil().clamp(1.0, 8192.0) as u32,
            size.height.ceil().clamp(1.0, 8192.0) as u32,
        );
        if self.web.borrow().is_none() && self.state.error.borrow().is_none() {
            let made = (|| {
                let context =
                    servo::SoftwareRenderingContext::new(pixels).map_err(|e| format!("{e:?}"))?;
                let url = engine::initial_url(&self.state)
                    .ok_or_else(|| "could not resolve initial Servo URL".to_owned())?;
                *self.state.allowed_url.borrow_mut() = url.to_string();
                Ok::<_, String>(
                    servo::WebViewBuilder::new(&engine::engine()?, Rc::new(context))
                        .delegate(self.state.clone())
                        .url(url)
                        .build(),
                )
            })();
            match made {
                Ok(web) => {
                    *self.web.borrow_mut() = Some(web);
                    for patch in std::mem::take(&mut *self.pending.borrow_mut()) {
                        self.update(&patch);
                    }
                }
                Err(error) => {
                    log::error!("Servo offscreen initialization failed: {error}");
                    *self.state.error.borrow_mut() = Some(error);
                }
            }
        }
        self.with_web(|web| web.resize(pixels));
        self.replay();
    }
    fn with_web(&self, f: impl FnOnce(&servo::WebView)) {
        #[cfg(all(
            any(feature = "gtk", feature = "qt"),
            target_os = "linux",
            not(target_env = "ohos")
        ))]
        let _graphics = match crate::linux_gl::Guard::enter() {
            Ok(guard) => guard,
            Err(error) => {
                log::error!("Servo graphics binding: {error}");
                return;
            }
        };
        let web = self.web.borrow().clone();
        if let Some(web) = web {
            f(&web);
            Waker.wake();
        }
    }
    fn update(&self, patch: &WebPatch) {
        if self.web.borrow().is_some() {
            self.with_web(|web| engine::update(&self.state, web, patch));
        } else if let WebPatch::Eval { req, .. } = patch {
            let error = self
                .state
                .error
                .borrow()
                .clone()
                .unwrap_or_else(|| "Servo view has not been laid out".to_owned());
            self.state.report(renderer::reply_event(
                *req,
                renderer::engine_error("ServoError", &error),
            ));
        } else {
            self.pending.borrow_mut().push(patch.clone());
        }
    }
    fn replay(&self) {
        let Some(node) = self.node.get() else {
            return;
        };
        let Some(bitmap) = self.bitmap.borrow().as_ref().cloned() else {
            return;
        };
        let size = self.size.get();
        with_tree(|tree| {
            tree.replay(
                node,
                vec![DrawOp::Image {
                    image: bitmap.id(),
                    rect: Rect::new(0.0, 0.0, size.width, size.height),
                    opacity: 1.0,
                }],
            )
        });
    }
    fn key(&self, event: &KeyEvent) {
        let named = match event.key.as_str() {
            "Enter" => Some(servo::NamedKey::Enter),
            "Tab" => Some(servo::NamedKey::Tab),
            "Backspace" => Some(servo::NamedKey::Backspace),
            "Delete" => Some(servo::NamedKey::Delete),
            "Escape" => Some(servo::NamedKey::Escape),
            "ArrowLeft" => Some(servo::NamedKey::ArrowLeft),
            "ArrowRight" => Some(servo::NamedKey::ArrowRight),
            "ArrowUp" => Some(servo::NamedKey::ArrowUp),
            "ArrowDown" => Some(servo::NamedKey::ArrowDown),
            _ => None,
        };
        let key = named
            .map(servo::Key::Named)
            .unwrap_or_else(|| servo::Key::Character(event.key.clone()));
        let mut modifiers = servo::Modifiers::empty();
        if event.modifiers & KeyEvent::SHIFT != 0 {
            modifiers |= servo::Modifiers::SHIFT;
        }
        if event.modifiers & KeyEvent::ALT != 0 {
            modifiers |= servo::Modifiers::ALT;
        }
        if event.modifiers & KeyEvent::PRIMARY != 0 {
            modifiers |= if cfg!(target_os = "macos") {
                servo::Modifiers::META
            } else {
                servo::Modifiers::CONTROL
            };
        }
        self.with_web(|web| {
            for state in [servo::KeyState::Down, servo::KeyState::Up] {
                web.notify_input_event(InputEvent::Keyboard(
                    servo::KeyboardEvent::new_without_event(
                        state,
                        key.clone(),
                        servo::Code::Unidentified,
                        servo::Location::Standard,
                        modifiers,
                        false,
                        false,
                    ),
                ));
            }
        });
    }
}
fn update(node: RNode, patch: &WebPatch) {
    if let Some(view) = get(node) {
        view.update(patch);
    }
}
fn release(node: RNode) {
    #[cfg(all(
        any(feature = "gtk", feature = "qt"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    let _graphics = match crate::linux_gl::Guard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            log::error!("Servo graphics binding: {error}");
            return;
        }
    };
    if let Some(view) = VIEWS.with(|views| views.borrow_mut().remove(&node)) {
        view.node.set(None);
        if view.state.props.session == 0 {
            engine::cancel_loads(view.state.node.get());
            view.web.borrow_mut().take();
        }
    }
    Waker.wake();
}
pub(crate) fn paint_dirty() {
    let views: Vec<_> = VIEWS.with(|views| views.borrow().values().cloned().collect());
    for view in views {
        if view.decoding.get() || !view.state.dirty.replace(false) {
            continue;
        }
        let Some(web) = view.web.borrow().clone() else {
            continue;
        };
        web.paint();
        let context = web.rendering_context();
        let size = context.size();
        let rect = euclid::Rect::new(
            euclid::Point2D::zero(),
            euclid::Size2D::new(size.width as i32, size.height as i32),
        );
        let Some(frame) = context.read_to_image(rect.into()) else {
            continue;
        };
        context.present();
        let mut png = Vec::new();
        if let Err(error) =
            PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::NoFilter)
                .write_image(
                    frame.as_raw(),
                    size.width,
                    size.height,
                    image::ExtendedColorType::Rgba8,
                )
        {
            log::error!("Servo frame encoding failed: {error}");
            continue;
        }
        view.decoding.set(true);
        day_core::decode_image_async(Arc::new(png), move |result| {
            view.decoding.set(false);
            if view.node.get().is_none() {
                return;
            }
            match result {
                Ok(bitmap) => {
                    *view.bitmap.borrow_mut() = Some(bitmap);
                    view.replay();
                }
                Err(error) => {
                    log::error!("Servo frame decoding failed: {error:?}");
                }
            }
            if view.state.dirty.get() {
                Waker.wake();
            }
        });
    }
}

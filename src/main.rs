#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;
extern crate cairo;
extern crate cairo_sys;
extern crate css_color_parser;
extern crate font_loader;
extern crate itertools;
extern crate pretty_env_logger;
extern crate x11;
extern crate xcb;
extern crate xcb_util;
extern crate xkbcommon;

use cairo::enums::{FontSlant, FontWeight};
use cairo::prelude::SurfaceExt;
use std::ffi::CStr;
use xkbcommon::xkb;

use std::collections::HashMap;
use std::iter::Iterator;

mod utils;

#[cfg(feature = "i3")]
extern crate i3ipc;

#[cfg(feature = "i3")]
mod wm_i3;

#[cfg(feature = "i3")]
use wm_i3 as wm;

#[derive(Debug)]
pub struct DesktopWindow {
    id: i64,
    title: String,
    pos: (i32, i32),
    size: (i32, i32),
}

pub struct RenderWindow<'a> {
    desktop_window: &'a DesktopWindow,
    cairo_context: cairo::Context,
    draw_pos: (f64, f64),
}

#[derive(Debug)]
pub struct AppConfig {
    pub font_family: String,
    pub font_size: f64,
    pub loaded_font: Vec<u8>,
    pub margin: f32,
    pub text_color: (f64, f64, f64, f64),
    pub bg_color: (f64, f64, f64, f64),
    pub fill: bool,
    pub horizontal_align: utils::HorizontalAlign,
    pub vertical_align: utils::VerticalAlign,
}

static HINT_CHARS: &'static str = "sadfjklewcmpgh";

#[cfg(any(feature = "i3", feature = "add_some_other_wm_here"))]
fn main() {
    let app_config = utils::parse_args();

    // Get the windows from each specific window manager implementation.
    let desktop_windows = wm::get_windows();

    let (conn, screen_num) = xcb::Connection::connect(None).unwrap();
    let setup = conn.get_setup();
    let screen = setup.roots().nth(screen_num as usize).unwrap();

    let values = [
        (
            xcb::CW_EVENT_MASK,
            xcb::EVENT_MASK_EXPOSURE
                | xcb::EVENT_MASK_KEY_PRESS
                | xcb::EVENT_MASK_BUTTON_PRESS
                | xcb::EVENT_MASK_BUTTON_RELEASE,
        ),
        (xcb::CW_OVERRIDE_REDIRECT, 1),
    ];

    let mut render_windows = HashMap::new();
    for desktop_window in &desktop_windows {
        // We need to estimate the font size before rendering because we want the window to only be
        // the size of the font.
        let hint = utils::get_next_hint(
            render_windows.keys().collect(),
            HINT_CHARS,
            desktop_windows.len(),
        );

        // Figure out how large the window actually needs to be.
        let text_extents =
            utils::extents_for_text(&hint, &app_config.font_family, app_config.font_size);
        let (width, height, margin_width, margin_height) = if app_config.fill {
            (
                desktop_window.size.0 as u16,
                desktop_window.size.1 as u16,
                (f64::from(desktop_window.size.0) - text_extents.width) / 2.0,
                (f64::from(desktop_window.size.1) - text_extents.height) / 2.0,
            )
        } else {
            let margin_factor = 1.0 + 0.2;
            (
                (text_extents.width * margin_factor).round() as u16,
                (text_extents.height * margin_factor).round() as u16,
                ((text_extents.width * margin_factor) - text_extents.width) / 2.0,
                ((text_extents.height * margin_factor) - text_extents.height) / 2.0,
            )
        };

        // Due to the way cairo lays out text, we'll have to calculate the actual coordinates to
        // put the cursor. See:
        // https://www.cairographics.org/samples/text_align_center/
        // https://www.cairographics.org/samples/text_extents/
        // https://www.cairographics.org/tutorial/#L1understandingtext
        let draw_pos = (
            margin_width - text_extents.x_bearing,
            text_extents.height + margin_height - (text_extents.height + text_extents.y_bearing),
        );

        debug!(
            "Spawning RenderWindow for this DesktopWindow: {:?}",
            desktop_window
        );

        let x = match app_config.horizontal_align {
            utils::HorizontalAlign::Left => desktop_window.pos.0 as i16,
            utils::HorizontalAlign::Center => {
                (desktop_window.pos.0 + desktop_window.size.0 / 2 - i32::from(width) / 2) as i16
            }
            utils::HorizontalAlign::Right => {
                (desktop_window.pos.0 + desktop_window.size.0 - i32::from(width)) as i16
            }
        };

        let y = match app_config.vertical_align {
            utils::VerticalAlign::Top => desktop_window.pos.1 as i16,
            utils::VerticalAlign::Center => {
                (desktop_window.pos.1 + desktop_window.size.1 / 2 - i32::from(height) / 2) as i16
            }
            utils::VerticalAlign::Bottom => {
                (desktop_window.pos.1 + desktop_window.size.1 - i32::from(height)) as i16
            }
        };

        let xcb_window_id = conn.generate_id();

        // Create the actual window.
        xcb::create_window(
            &conn,
            xcb::COPY_FROM_PARENT as u8,
            xcb_window_id,
            screen.root(),
            x,
            y,
            width,
            height,
            0,
            xcb::WINDOW_CLASS_INPUT_OUTPUT as u16,
            screen.root_visual(),
            &values,
        );

        xcb::map_window(&conn, xcb_window_id);

        // Set title.
        let title = crate_name!();
        xcb::change_property(
            &conn,
            xcb::PROP_MODE_REPLACE as u8,
            xcb_window_id,
            xcb::ATOM_WM_NAME,
            xcb::ATOM_STRING,
            8,
            title.as_bytes(),
        );

        conn.flush();

        let mut visual = utils::find_visual(&conn, screen.root_visual()).unwrap();
        let cairo_xcb_conn = unsafe {
            cairo::XCBConnection::from_raw_none(
                conn.get_raw_conn() as *mut cairo_sys::xcb_connection_t
            )
        };
        let cairo_xcb_drawable = cairo::XCBDrawable(xcb_window_id);
        let raw_visualtype = &mut visual.base as *mut xcb::ffi::xcb_visualtype_t;
        let cairo_xcb_visual = unsafe {
            cairo::XCBVisualType::from_raw_none(raw_visualtype as *mut cairo_sys::xcb_visualtype_t)
        };
        let surface = <cairo::Surface as cairo::XCBSurface>::create(
            &cairo_xcb_conn,
            &cairo_xcb_drawable,
            &cairo_xcb_visual,
            width.into(),
            height.into(),
        );
        let cairo_context = cairo::Context::new(&surface);

        let render_window = RenderWindow {
            desktop_window,
            cairo_context,
            draw_pos,
        };

        render_windows.insert(hint, render_window);
    }

    // Receive keyboard events.
    let grab_keyboard_cookie = xcb::xproto::grab_keyboard(
        &conn,
        true,
        screen.root(),
        xcb::CURRENT_TIME,
        xcb::GRAB_MODE_ASYNC as u8,
        xcb::GRAB_MODE_ASYNC as u8,
    );
    grab_keyboard_cookie
        .get_reply()
        .expect("Couldn't grab keyboard");

    // Receive mouse events.
    let grab_pointer_cookie = xcb::xproto::grab_pointer(
        &conn,
        true,
        screen.root(),
        xcb::EVENT_MASK_BUTTON_PRESS as u16,
        xcb::GRAB_MODE_ASYNC as u8,
        xcb::GRAB_MODE_ASYNC as u8,
        xcb::NONE,
        xcb::NONE,
        xcb::CURRENT_TIME,
    );
    grab_pointer_cookie
        .get_reply()
        .expect("Couldn't grab mouse");

    let mut closed = false;
    while !closed {
        let event = conn.wait_for_event();
        match event {
            None => {
                closed = true;
            }
            Some(event) => {
                let r = event.response_type();
                match r {
                    xcb::EXPOSE => {
                        for (hint, rw) in &render_windows {
                            rw.cairo_context.set_source_rgba(
                                app_config.bg_color.0,
                                app_config.bg_color.1,
                                app_config.bg_color.2,
                                app_config.bg_color.3,
                            );
                            rw.cairo_context.paint();
                            rw.cairo_context.select_font_face(
                                &app_config.font_family,
                                FontSlant::Normal,
                                FontWeight::Normal,
                            );
                            rw.cairo_context.set_font_size(app_config.font_size);
                            rw.cairo_context.move_to(rw.draw_pos.0, rw.draw_pos.1);
                            rw.cairo_context.set_source_rgba(
                                app_config.text_color.0,
                                app_config.text_color.1,
                                app_config.text_color.2,
                                app_config.text_color.3,
                            );
                            rw.cairo_context.show_text(&hint);
                            rw.cairo_context.get_target().flush();
                            conn.flush();
                        }
                    }
                    xcb::BUTTON_PRESS => {
                        closed = true;
                    }
                    xcb::KEY_PRESS => {
                        let key_press: &xcb::KeyPressEvent = unsafe { xcb::cast_event(&event) };

                        let syms = xcb_util::keysyms::KeySymbols::new(&conn);
                        let ksym = syms.press_lookup_keysym(key_press, 0);
                        let kstr = unsafe {
                            CStr::from_ptr(x11::xlib::XKeysymToString(ksym.into()))
                                .to_str()
                                .expect("Couldn't create Rust string from C string")
                        };
                        if ksym == xkb::KEY_Escape {
                            closed = true;
                        }
                        if let Some(rw) = &render_windows.get(kstr) {
                            wm::focus_window(&rw.desktop_window);
                            closed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(not(any(feature = "i3", feature = "add_some_other_wm_here")))]
fn main() {
    eprintln!(
        "You need to enable to enabe support for at least one window manager.\n
Currently supported:
    --features i3"
    );
}

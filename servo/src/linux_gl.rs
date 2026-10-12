// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Preserve the toolkit's thread-local graphics binding around Servo calls.
//!
//! Qt can leave GLX current when invoking Day. libglvnd rejects binding Servo's EGL
//! context while GLX is current (EGL_BAD_ACCESS), and rejects the reverse transition
//! too. GTK may instead leave an EGL context current. Restore both APIs and EGL's
//! selected client API before returning to the toolkit, including during unwinding.
use std::{ffi::c_void, marker::PhantomData, os::raw::c_ulong, ptr, rc::Rc};

type Handle = *mut c_void;
const EGL_DRAW: i32 = 0x3059;
const EGL_READ: i32 = 0x305A;

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetCurrentDisplay() -> Handle;
    fn eglGetCurrentContext() -> Handle;
    fn eglGetCurrentSurface(which: i32) -> Handle;
    fn eglQueryAPI() -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglMakeCurrent(display: Handle, draw: Handle, read: Handle, context: Handle) -> u32;
}
#[link(name = "GL")]
unsafe extern "C" {
    fn glXGetCurrentDisplay() -> Handle;
    fn glXGetCurrentContext() -> Handle;
    fn glXGetCurrentDrawable() -> c_ulong;
    fn glXGetCurrentReadDrawable() -> c_ulong;
    fn glXMakeContextCurrent(display: Handle, draw: c_ulong, read: c_ulong, context: Handle)
    -> i32;
}

#[must_use]
pub(crate) struct Guard {
    egl_display: Handle,
    egl_context: Handle,
    egl_draw: Handle,
    egl_read: Handle,
    egl_api: u32,
    glx_display: Handle,
    glx_context: Handle,
    glx_draw: c_ulong,
    glx_read: c_ulong,
    // The borrowed bindings must be restored on the thread that owns them.
    _thread: PhantomData<Rc<()>>,
}

impl Guard {
    pub(crate) fn enter() -> Result<Self, &'static str> {
        // SAFETY: These queries return bindings owned by this thread. The guard never
        // destroys them, and Servo does not own the toolkit's context or surfaces.
        unsafe {
            let guard = Self {
                egl_display: eglGetCurrentDisplay(),
                egl_context: eglGetCurrentContext(),
                egl_draw: eglGetCurrentSurface(EGL_DRAW),
                egl_read: eglGetCurrentSurface(EGL_READ),
                egl_api: eglQueryAPI(),
                glx_display: glXGetCurrentDisplay(),
                glx_context: glXGetCurrentContext(),
                glx_draw: glXGetCurrentDrawable(),
                glx_read: glXGetCurrentReadDrawable(),
                _thread: PhantomData,
            };
            if !guard.glx_context.is_null()
                && glXMakeContextCurrent(guard.glx_display, 0, 0, ptr::null_mut()) == 0
            {
                return Err("could not release the toolkit's GLX context");
            }
            if !guard.egl_context.is_null()
                && eglMakeCurrent(
                    guard.egl_display,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ) == 0
            {
                return Err("could not release the toolkit's EGL context");
            }
            Ok(guard)
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: Guard is !Send/!Sync. Its saved handles are borrowed for a synchronous
        // Servo call; they remain alive until control returns to the toolkit.
        unsafe {
            let display = eglGetCurrentDisplay();
            if !display.is_null() {
                eglMakeCurrent(display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            }
            eglBindAPI(self.egl_api);
            if !self.egl_context.is_null() {
                eglMakeCurrent(
                    self.egl_display,
                    self.egl_draw,
                    self.egl_read,
                    self.egl_context,
                );
            }
            if !self.glx_context.is_null() {
                glXMakeContextCurrent(
                    self.glx_display,
                    self.glx_draw,
                    self.glx_read,
                    self.glx_context,
                );
            }
        }
    }
}

// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Standalone graphics regression: rustc --test tests/linux/gl-context.rs; run under Xvfb.
//! Uses the adapter's actual guard without compiling the Servo engine.
#![cfg(target_os = "linux")]
#[path = "../../servo/src/linux_gl.rs"]
mod linux_gl;

use std::{ffi::c_void, ptr};
type Handle = *mut c_void;
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const u8) -> Handle;
    fn XCloseDisplay(display: Handle) -> i32;
    fn XFree(data: Handle) -> i32;
}
#[link(name = "GL")]
unsafe extern "C" {
    fn glXChooseFBConfig(
        display: Handle,
        screen: i32,
        attrs: *const i32,
        n: *mut i32,
    ) -> *mut Handle;
    fn glXCreateNewContext(
        display: Handle,
        config: Handle,
        ty: i32,
        share: Handle,
        direct: i32,
    ) -> Handle;
    fn glXCreatePbuffer(display: Handle, config: Handle, attrs: *const i32) -> u64;
    fn glXMakeContextCurrent(display: Handle, draw: u64, read: u64, context: Handle) -> i32;
    fn glXGetCurrentContext() -> Handle;
    fn glXGetCurrentDrawable() -> u64;
    fn glXDestroyContext(display: Handle, context: Handle);
    fn glXDestroyPbuffer(display: Handle, surface: u64);
}
#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetDisplay(native: Handle) -> Handle;
    fn eglInitialize(display: Handle, major: *mut i32, minor: *mut i32) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglQueryAPI() -> u32;
    fn eglChooseConfig(
        display: Handle,
        attrs: *const i32,
        configs: *mut Handle,
        size: i32,
        n: *mut i32,
    ) -> u32;
    fn eglCreateContext(
        display: Handle,
        config: Handle,
        share: Handle,
        attrs: *const i32,
    ) -> Handle;
    fn eglCreatePbufferSurface(display: Handle, config: Handle, attrs: *const i32) -> Handle;
    fn eglMakeCurrent(display: Handle, draw: Handle, read: Handle, context: Handle) -> u32;
    fn eglGetCurrentContext() -> Handle;
    fn eglGetCurrentSurface(which: i32) -> Handle;
    fn eglGetError() -> i32;
    fn eglDestroyContext(display: Handle, context: Handle) -> u32;
    fn eglDestroySurface(display: Handle, surface: Handle) -> u32;
    fn eglTerminate(display: Handle) -> u32;
}

#[test]
fn restores_glx_egl_and_unbound_contexts_after_servo_and_unwinding() {
    // SAFETY: This test owns all displays, pbuffers and contexts until cleanup, and
    // confines every graphics operation (including guard destruction) to this thread.
    unsafe {
        let x = XOpenDisplay(ptr::null());
        assert!(!x.is_null(), "run this test under Xvfb");
        let mut count = 0;
        let configs = glXChooseFBConfig(x, 0, [0x8010, 4, 0x8011, 1, 0].as_ptr(), &mut count);
        assert!(count > 0);
        let glx = glXCreateNewContext(x, *configs, 0x8014, ptr::null_mut(), 1);
        let pbuffer = glXCreatePbuffer(x, *configs, [0x8041, 16, 0x8040, 16, 0].as_ptr());
        XFree(configs.cast());
        assert!(!glx.is_null());
        assert_ne!(pbuffer, 0);
        let egl = eglGetDisplay(x);
        assert_ne!(eglInitialize(egl, ptr::null_mut(), ptr::null_mut()), 0);
        const OPENGL: u32 = 0x30A2;
        const GLES: u32 = 0x30A0;
        assert_ne!(eglBindAPI(OPENGL), 0);
        let mut config = ptr::null_mut();
        assert_ne!(
            eglChooseConfig(
                egl,
                [0x3033, 1, 0x3040, 8, 0x3038].as_ptr(),
                &mut config,
                1,
                &mut count
            ),
            0
        );
        assert_eq!(count, 1);
        let context = eglCreateContext(egl, config, ptr::null_mut(), [0x3038].as_ptr());
        let surface =
            eglCreatePbufferSurface(egl, config, [0x3057, 16, 0x3056, 16, 0x3038].as_ptr());
        assert!(!context.is_null() && !surface.is_null());

        // Reproduce the original failure before verifying the guard fixes it.
        assert_ne!(glXMakeContextCurrent(x, pbuffer, pbuffer, glx), 0);
        assert_eq!(eglMakeCurrent(egl, surface, surface, context), 0);
        assert_eq!(
            eglGetError(),
            0x3002,
            "EGL_BAD_ACCESS with GLX still current"
        );
        {
            let _guard = linux_gl::Guard::enter().unwrap();
            assert!(glXGetCurrentContext().is_null());
            assert_ne!(eglMakeCurrent(egl, surface, surface, context), 0);
            // Nested entry must preserve Servo's own context as well.
            {
                let _nested = linux_gl::Guard::enter().unwrap();
            }
            assert_eq!(eglGetCurrentContext(), context);
        }
        assert_eq!(glXGetCurrentContext(), glx);
        assert_eq!(glXGetCurrentDrawable(), pbuffer);
        assert!(eglGetCurrentContext().is_null());
        assert_ne!(glXMakeContextCurrent(x, 0, 0, ptr::null_mut()), 0);

        // Also restore an EGL toolkit binding when a Servo callback unwinds.
        assert_ne!(eglMakeCurrent(egl, surface, surface, context), 0);
        let result = std::panic::catch_unwind(|| {
            let _guard = linux_gl::Guard::enter().unwrap();
            assert_ne!(eglBindAPI(GLES), 0);
            panic!("synthetic callback failure");
        });
        assert!(result.is_err());
        assert_eq!(eglQueryAPI(), OPENGL);
        assert_eq!(eglGetCurrentContext(), context);
        assert_eq!(eglGetCurrentSurface(0x3059), surface);
        assert_ne!(
            eglMakeCurrent(egl, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()),
            0
        );
        {
            let _guard = linux_gl::Guard::enter().unwrap();
            assert_ne!(eglMakeCurrent(egl, surface, surface, context), 0);
        }
        assert!(eglGetCurrentContext().is_null());
        assert!(glXGetCurrentContext().is_null());
        assert_ne!(eglDestroySurface(egl, surface), 0);
        assert_ne!(eglDestroyContext(egl, context), 0);
        assert_ne!(eglTerminate(egl), 0);
        glXDestroyPbuffer(x, pbuffer);
        glXDestroyContext(x, glx);
        XCloseDisplay(x);
    }
}

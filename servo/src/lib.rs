// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Experimental Servo adapter for day-piece-webview. Enable `servo` and an available host,
//! then call [`register`] on the UI thread before constructing views. See `docs/servo.md`.
pub use day_piece_webview::renderer::RegistrationError;

#[cfg(all(
    feature = "servo",
    any(feature = "gtk", feature = "qt"),
    target_os = "linux",
    not(target_env = "ohos")
))]
mod linux_gl;

#[cfg(all(feature = "servo", target_os = "windows", not(target_env = "msvc")))]
compile_error!("The Servo adapter requires the MSVC Windows toolchain (xaml or winui).");

#[cfg(all(feature = "servo", feature = "appkit", target_os = "macos"))]
mod appkit;
#[cfg(all(
    feature = "servo",
    any(
        feature = "appkit",
        feature = "gtk",
        feature = "qt",
        feature = "xaml",
        feature = "material",
        feature = "arkui"
    ),
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows",
        target_os = "android"
    )
))]
mod engine;
#[cfg(all(
    feature = "servo",
    any(
        feature = "gtk",
        feature = "qt",
        feature = "xaml",
        feature = "material",
        feature = "arkui"
    ),
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows",
        target_os = "android"
    )
))]
mod surface;

/// Register the adapter without initializing Servo. Repeated calls are harmless.
/// Returns an error on unsupported builds or if another adapter has claimed the renderer.
pub fn register() -> Result<(), RegistrationError> {
    #[cfg(all(feature = "servo", feature = "appkit", target_os = "macos"))]
    {
        use day_piece_webview::{
            WebRenderer,
            renderer::{Capabilities, appkit::Host},
        };
        use day_spec::Support;
        static HOST: Host = Host {
            capabilities: Capabilities {
                private_browsing: Support::Unsupported,
                profiles: Support::Unsupported,
                clear_data: Support::Unsupported,
            },
            make: appkit::make,
            update: appkit::update,
            release: appkit::release,
        };
        let mtm = objc2::MainThreadMarker::new().ok_or(RegistrationError::WrongThread)?;
        if day_core::toolkit_key() == Some("appkit") {
            return day_piece_webview::renderer::appkit::register(mtm, WebRenderer::Servo, &HOST);
        }
    }
    #[cfg(all(
        feature = "servo",
        any(
            feature = "gtk",
            feature = "qt",
            feature = "xaml",
            feature = "material",
            feature = "arkui"
        ),
        any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows",
            target_os = "android"
        )
    ))]
    return surface::register();
    #[allow(unreachable_code)] // Feature-selected host branches return above.
    Err(RegistrationError::Unsupported)
}

#[cfg(all(test, feature = "servo", feature = "appkit", target_os = "macos"))]
mod appkit_tests {
    #[test]
    fn registration_rejects_a_worker_thread_without_advertising_the_engine() {
        let result = std::thread::spawn(super::register).join().unwrap();
        assert_eq!(result, Err(super::RegistrationError::WrongThread));
        assert!(!day_piece_webview::WebRenderer::Servo.available());
    }
}

#[cfg(all(test, not(feature = "servo")))]
mod tests {
    #[test]
    fn unsupported_build_does_not_advertise_servo() {
        assert_eq!(
            super::register(),
            Err(super::RegistrationError::Unsupported)
        );
        assert!(!day_piece_webview::WebRenderer::Servo.available());
    }
}

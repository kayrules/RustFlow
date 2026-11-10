//! The Mac window showing the peer's screen.
//!
//! An `AVSampleBufferDisplayLayer` fills the window; it decodes H.264 in
//! hardware and shows each frame as soon as it is queued.

use std::ffi::c_void;

use anyhow::{Result, bail};
use bytes::Bytes;
use objc2::encode::{Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::platform::macos::WindowExtMacOS;
use tao::window::{Window, WindowBuilder, WindowId};
use tracing::{debug, info, warn};

use super::{Event, ParameterSets};
use crate::input::ViewArea;

/// What the tray should do after a stream event.
pub enum Action {
    None,
    /// The picture size is known or changed: update the input area.
    Resized,
    /// The decoder failed; get a fresh stream (it starts with a key frame).
    Restart,
}

pub struct ScreenWindow {
    window: Window,
    layer: Retained<AnyObject>,
    format: Option<FormatDescription>,
    /// Decoding resumes only at a key frame after a failure.
    need_key: bool,
    label: String,
    focused: bool,
    sized: bool,
}

impl ScreenWindow {
    pub fn open<T: 'static>(target: &EventLoopWindowTarget<T>, label: &str) -> Result<Self> {
        let window = WindowBuilder::new()
            .with_title(format!("{label} — connecting…"))
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_min_inner_size(LogicalSize::new(320.0, 200.0))
            .build(target)?;
        let layer = unsafe { attach_layer(&window)? };
        window.set_focus();
        Ok(Self { window, layer, format: None, need_key: true, label: label.to_owned(), focused: true, sized: false })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn focus(&self) {
        self.window.set_focus();
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Where the picture is, for input; `None` unless focused and playing.
    pub fn area(&self) -> Option<ViewArea> {
        if !self.focused || self.window.is_minimized() {
            return None;
        }
        let (video_width, video_height) = self.format.as_ref()?.dimensions();
        let scale = self.window.scale_factor();
        let pos = self.window.inner_position().ok()?.to_logical::<f64>(scale);
        let size = self.window.inner_size().to_logical::<f64>(scale);
        Some(ViewArea { x: pos.x, y: pos.y, width: size.width, height: size.height, video_width, video_height })
    }

    pub fn handle(&mut self, event: Event) -> Action {
        match event {
            Event::Status(text) => {
                self.window.set_title(&format!("{} — {text}", self.label));
                Action::None
            }
            Event::Format(sets) => match FormatDescription::new(&sets) {
                Ok(format) => {
                    let (w, h) = format.dimensions();
                    info!("screen: picture is {w}x{h}");
                    self.window.set_title(&self.label);
                    self.format = Some(format);
                    self.need_key = true;
                    if !self.sized {
                        self.sized = true;
                        self.fit_to(w, h);
                    }
                    Action::Resized
                }
                Err(e) => {
                    warn!("screen: {e:#}");
                    Action::Restart
                }
            },
            Event::Frame { key, data } => self.show(key, &data),
        }
    }

    fn show(&mut self, key: bool, data: &Bytes) -> Action {
        let Some(format) = &self.format else { return Action::None };
        if self.need_key && !key {
            return Action::None;
        }
        self.need_key = false;
        if let Err(e) = unsafe { enqueue(&self.layer, format, data) } {
            warn!("screen: {e:#}");
        }
        let status: isize = unsafe { msg_send![&*self.layer, status] };
        let must_flush: bool = unsafe { msg_send![&*self.layer, requiresFlushToResumeDecoding] };
        if status == STATUS_FAILED || must_flush {
            debug!("screen: decoder needs a restart");
            unsafe {
                let _: () = msg_send![&*self.layer, flush];
            }
            self.need_key = true;
            return Action::Restart;
        }
        Action::None
    }

    /// First picture: size the window to its shape, at most 85% of the screen.
    fn fit_to(&self, w: f64, h: f64) {
        let Some(monitor) = self.window.current_monitor() else { return };
        let screen = monitor.size().to_logical::<f64>(monitor.scale_factor());
        let scale = (screen.width * 0.85 / w).min(screen.height * 0.85 / h).min(1.0);
        self.window.set_inner_size(LogicalSize::new(w * scale, h * scale));
    }
}

const STATUS_FAILED: isize = 2; // AVQueuedSampleBufferRenderingStatusFailed

/// Give the Dock the menu bar icon (the bundle has no icon of its own).
pub fn set_dock_icon() {
    use core_graphics::base::{kCGImageAlphaLast, kCGRenderingIntentDefault};
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::data_provider::CGDataProvider;
    use core_graphics::image::CGImage;
    use objc2::rc::Allocated;
    use objc2_foundation::NSSize;

    const SIZE: usize = 512;
    let pixels = crate::icon::rgba(SIZE as u32, crate::status::Light::Connected);
    let provider = CGDataProvider::from_buffer(std::sync::Arc::new(pixels));
    let image = CGImage::new(
        SIZE,
        SIZE,
        8,
        32,
        SIZE * 4,
        &CGColorSpace::create_device_rgb(),
        kCGImageAlphaLast,
        &provider,
        true,
        kCGRenderingIntentDefault,
    );
    // A foreign-types reference is the C pointer itself.
    let cg_image = &*image as *const core_graphics::image::CGImageRef as *mut CGImageOpaque;
    unsafe {
        let alloc: Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
        // Zero size: use the image's own size.
        let ns_image: Retained<AnyObject> = msg_send![alloc, initWithCGImage: cg_image, size: NSSize::new(0.0, 0.0)];
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, setApplicationIconImage: &*ns_image];
    }
}

/// Make the window's view host an `AVSampleBufferDisplayLayer`.
unsafe fn attach_layer(window: &Window) -> Result<Retained<AnyObject>> {
    unsafe {
        let view = window.ns_view() as *mut AnyObject;
        let ns_window = window.ns_window() as *mut AnyObject;
        if view.is_null() || ns_window.is_null() {
            bail!("window has no view");
        }
        let layer: Retained<AnyObject> = msg_send![class!(AVSampleBufferDisplayLayer), new];
        let _: () = msg_send![&*layer, setVideoGravity: AVLayerVideoGravityResizeAspect];
        // Layer-hosting view: set the layer first, then turn layers on.
        let _: () = msg_send![view, setLayer: &*layer];
        let _: () = msg_send![view, setWantsLayer: true];
        // Black bars where the picture doesn't fill the window.
        let black: *mut AnyObject = msg_send![class!(NSColor), blackColor];
        let _: () = msg_send![ns_window, setBackgroundColor: black];
        Ok(layer)
    }
}

/// Queue one encoded picture for display right away.
unsafe fn enqueue(layer: &AnyObject, format: &FormatDescription, data: &[u8]) -> Result<()> {
    unsafe {
        let mut block: CMBlockBufferRef = std::ptr::null_mut();
        check(
            CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null(),
                std::ptr::null_mut(),
                data.len(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                data.len(),
                ASSURE_MEMORY_NOW,
                &mut block,
            ),
            "CMBlockBufferCreateWithMemoryBlock",
        )?;
        let _block = Released(block as CFTypeRef);
        check(
            CMBlockBufferReplaceDataBytes(data.as_ptr().cast(), block, 0, data.len()),
            "CMBlockBufferReplaceDataBytes",
        )?;
        let mut sample: CMSampleBufferRef = std::ptr::null_mut();
        let size = data.len();
        check(
            CMSampleBufferCreateReady(
                std::ptr::null(),
                block,
                format.0,
                1,
                0,
                std::ptr::null(),
                1,
                &size,
                &mut sample,
            ),
            "CMSampleBufferCreateReady",
        )?;
        let _sample = Released(sample as CFTypeRef);
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, 1);
        if !attachments.is_null() {
            let dict = CFArrayGetValueAtIndex(attachments, 0);
            CFDictionarySetValue(dict, kCMSampleAttachmentKey_DisplayImmediately, kCFBooleanTrue);
        }
        let _: () = msg_send![layer, enqueueSampleBuffer: sample];
        Ok(())
    }
}

/// A CoreMedia H.264 format description (decoder setup).
struct FormatDescription(CMFormatDescriptionRef);

impl FormatDescription {
    fn new(ps: &ParameterSets) -> Result<Self> {
        let pointers: Vec<*const u8> = ps.sets.iter().map(|s| s.as_ptr()).collect();
        let sizes: Vec<usize> = ps.sets.iter().map(|s| s.len()).collect();
        let mut desc: CMFormatDescriptionRef = std::ptr::null_mut();
        check(
            unsafe {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    std::ptr::null(),
                    pointers.len(),
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    ps.length_size as i32,
                    &mut desc,
                )
            },
            "CMVideoFormatDescriptionCreateFromH264ParameterSets",
        )?;
        Ok(Self(desc))
    }

    fn dimensions(&self) -> (f64, f64) {
        let d = unsafe { CMVideoFormatDescriptionGetDimensions(self.0) };
        (d.width as f64, d.height as f64)
    }
}

impl Drop for FormatDescription {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

/// Releases a CoreFoundation object when dropped.
struct Released(CFTypeRef);

impl Drop for Released {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn check(status: OSStatus, what: &str) -> Result<()> {
    if status != 0 {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

// ---- CoreMedia / CoreFoundation / AVFoundation ----

type OSStatus = i32;
type CFTypeRef = *const c_void;

#[repr(C)]
struct OpaqueFormatDescription([u8; 0]);
#[repr(C)]
struct OpaqueBlockBuffer([u8; 0]);
/// Named like the C struct so Objective-C type checks match.
#[repr(C)]
#[allow(non_camel_case_types)]
struct opaqueCMSampleBuffer([u8; 0]);

#[repr(C)]
struct CGImageOpaque([u8; 0]);

unsafe impl RefEncode for CGImageOpaque {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}

unsafe impl RefEncode for opaqueCMSampleBuffer {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("opaqueCMSampleBuffer", &[]));
}

type CMFormatDescriptionRef = *mut OpaqueFormatDescription;
type CMBlockBufferRef = *mut OpaqueBlockBuffer;
type CMSampleBufferRef = *mut opaqueCMSampleBuffer;

#[repr(C)]
struct CMVideoDimensions {
    width: i32,
    height: i32,
}

const ASSURE_MEMORY_NOW: u32 = 1; // kCMBlockBufferAssureMemoryNowFlag

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_unit_header_length: i32,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetDimensions(desc: CMFormatDescriptionRef) -> CMVideoDimensions;
    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: CFTypeRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFTypeRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        destination: CMBlockBufferRef,
        offset: usize,
        length: usize,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFTypeRef,
        data_buffer: CMBlockBufferRef,
        format: CMFormatDescriptionRef,
        num_samples: isize,
        num_timing_entries: isize,
        timing: *const c_void,
        num_size_entries: isize,
        sizes: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
    fn CMSampleBufferGetSampleAttachmentsArray(sample: CMSampleBufferRef, create: u8) -> CFTypeRef;
    static kCMSampleAttachmentKey_DisplayImmediately: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFDictionarySetValue(dict: CFTypeRef, key: CFTypeRef, value: CFTypeRef);
    static kCFBooleanTrue: CFTypeRef;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVLayerVideoGravityResizeAspect: &'static AnyObject;
}

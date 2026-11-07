use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use libc::{c_void, mmap, munmap, pollfd, PROT_READ, MAP_SHARED, MAP_FAILED, POLLIN};
use anyhow::Result;

// Correct ioctl numbers from kernel headers (x86_64)
// NOTE: This module is x86_64-only. ioctl numbers and struct layouts are ABI-dependent.
// For ARM/aarch64 support, replace with the `v4l` or `v4l2-sys` crates.
const VIDIOC_G_FMT: u64 = 0xc0d05604;
const VIDIOC_REQBUFS: u64 = 0xc0145608;
const VIDIOC_QUERYBUF: u64 = 0xc0585609;
const VIDIOC_QBUF: u64 = 0xc058560f;
const VIDIOC_DQBUF: u64 = 0xc0585611;
const VIDIOC_STREAMON: u64 = 0x40045612;
const VIDIOC_STREAMOFF: u64 = 0x40045613;
const VIDIOC_S_EXT_CTRLS: u64 = 0xc0205648;

/// UVC 1.5 region of interest (camera terminal control 0x14), as exposed by
/// uvcvideo: `V4L2_CID_CAMERA_UVC_BASE + 1` and `+ 2`.
const V4L2_CID_UVC_REGION_OF_INTEREST_RECT: u32 = 0x0098_1ae1;
const V4L2_CID_UVC_REGION_OF_INTEREST_AUTO: u32 = 0x0098_1ae2;
/// Bit 0 of the auto-controls bitmask: auto-exposure meters on the region.
const V4L2_UVC_REGION_OF_INTEREST_AUTO_EXPOSURE: u32 = 1;

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_MEMORY_MMAP: u32 = 1;

/// v4l2_fourcc('G','R','E','Y') — 8-bit greyscale, what Windows Hello IR
/// sensors expose. The capture path widens each byte to u16 directly, so any
/// other format (YUYV, MJPEG) would be fed to the model as noise.
const V4L2_PIX_FMT_GREY: u32 = 0x5945_5247;

/// Refuse implausible geometry from `G_FMT` before it sizes an allocation.
const MAX_DIMENSION: u32 = 8192;

/// Buffers to request from the driver.
///
/// One is not enough. Between `DQBUF` and the following `QBUF` the driver has
/// nowhere to put an incoming frame, so it drops it — which means two
/// successive captures are not necessarily adjacent frames. On a sensor that
/// strobes its illuminator on alternate frames, that makes the phase of what
/// you get unpredictable, and "take the brighter of two" can hand back two
/// unlit frames in a row. A small ring keeps a buffer queued at all times.
const BUFFER_COUNT: u32 = 4;

/// Stream restarts allowed within one `capture_frame` call before giving up.
///
/// See [`Camera::capture_frame`]: in darkness the reference sensor blanks its
/// output about 0.8 s after each stream start, and a restart buys another
/// 0.8 s of real frames. A handful per call keeps a sensor that blanks for
/// some other reason from looping forever.
const MAX_STREAM_RESTARTS: u32 = 3;

// Kernel struct v4l2_format: type(4) + padding(4) + union raw_data[200] = 208 bytes
#[repr(C)]
struct v4l2_format {
    type_: u32,
    _pad: [u8; 4],
    raw: [u8; 200],
}

fn make_v4l2_format(type_: u32, width: u32, height: u32, pixelformat: u32) -> v4l2_format {
    let mut fmt = v4l2_format {
        type_,
        _pad: [0; 4],
        raw: [0; 200],
    };
    let pix: &mut v4l2_pix_format = unsafe { &mut *(fmt.raw.as_mut_ptr() as *mut v4l2_pix_format) };
    pix.width = width;
    pix.height = height;
    pix.pixelformat = pixelformat;
    fmt
}

#[repr(C)]
struct v4l2_pix_format {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct v4l2_requestbuffers {
    count: u32,
    type_: u32,
    memory: u32,
    reserved: [u32; 2],
}

// Kernel struct v4l2_buffer: 88 bytes
#[repr(C)]
struct v4l2_buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: [i64; 2],      // struct timeval
    timecode: [u32; 4],       // struct v4l2_timecode: type, flags, frames, seconds, minutes, hours, userbits[4] → packed as 4 u32
    sequence: u32,
    memory: u32,
    m: u64,                   // union { offset, userptr, planes, fd }
    length: u32,
    reserved2: u32,
    reserved: u32,
}

#[repr(C)]
struct v4l2_rect {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

// Kernel struct v4l2_ext_control is packed: id, size, reserved2, then a
// 64-bit union holding either the value or a payload pointer. 20 bytes.
#[repr(C, packed)]
struct v4l2_ext_control {
    id: u32,
    size: u32,
    reserved2: u32,
    value_or_ptr: u64,
}

// Kernel struct v4l2_ext_controls: 32 bytes on x86_64.
#[repr(C)]
struct v4l2_ext_controls {
    which: u32,
    count: u32,
    error_idx: u32,
    request_fd: i32,
    reserved: [u32; 1],
    controls: *mut v4l2_ext_control,
}

pub struct IrFrame {
    pub data: Vec<u16>,
    pub width: u32,
    pub height: u32,
}

impl IrFrame {
    /// Mean sample value, 0.0–65535.0. Accumulated in f64 because a 640x400
    /// frame sums a quarter of a million terms.
    pub fn mean_intensity(&self) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().map(|&v| v as f64).sum::<f64>() / self.data.len() as f64
    }

    /// True if every sample has the same value.
    ///
    /// No real sensor frame does this: even a lens cap leaves read noise. The
    /// reference ASUS sensor sends exactly this, a solid fill of 144, once it
    /// has decided the scene is dark (see [`Camera::capture_frame`]).
    pub fn is_flat_fill(&self) -> bool {
        match self.data.split_first() {
            Some((first, rest)) => rest.iter().all(|v| v == first),
            None => false,
        }
    }
}

/// One mmap'd capture buffer.
struct MappedBuffer {
    ptr: *mut c_void,
    length: usize,
}

pub struct Camera {
    fd: OwnedFd,
    buffers: Vec<MappedBuffer>,
    width: u32,
    height: u32,
    stream_on: bool,
    /// Stream restarts caused by flat-fill frames since `open`.
    restarts: u32,
    /// Whether the exposure region has been set since `open`.
    roi_set: bool,
}

unsafe impl Send for Camera {}

impl Camera {
    pub fn open(device_path: &str) -> Result<Self> {
        let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
            .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        // Query current format instead of setting it.
        // VIDIOC_S_FMT triggers sensor init on IR cameras (~2s delay).
        // The camera is already configured correctly, so G_FMT is instant.
        let mut fmt = make_v4l2_format(V4L2_BUF_TYPE_VIDEO_CAPTURE, 0, 0, 0);
        ioctl(fd.as_raw_fd(), VIDIOC_G_FMT, &mut fmt as *mut _ as *mut c_void)?;
        let pix: &v4l2_pix_format = unsafe { &*(fmt.raw.as_ptr() as *const v4l2_pix_format) };
        let width = pix.width;
        let height = pix.height;
        let pixelformat = pix.pixelformat;

        // Validate what the driver reports rather than assuming it. Pointed at
        // an ordinary RGB webcam this would otherwise silently reinterpret
        // YUYV or MJPEG bytes as greyscale and compare the noise against a
        // real template.
        if pixelformat != V4L2_PIX_FMT_GREY {
            let fourcc: String = pixelformat
                .to_le_bytes()
                .iter()
                .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
                .collect();
            return Err(anyhow::anyhow!(
                "{} reports pixel format '{}' ({:#010x}); this tool requires raw 8-bit GREY \
                 from an IR sensor",
                device_path,
                fourcc,
                pixelformat
            ));
        }
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(anyhow::anyhow!(
                "{} reports implausible frame geometry {}x{}",
                device_path,
                width,
                height
            ));
        }

        let mut reqbuf = v4l2_requestbuffers {
            count: BUFFER_COUNT,
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            reserved: [0, 0],
        };
        ioctl(fd.as_raw_fd(), VIDIOC_REQBUFS, &mut reqbuf as *mut _ as *mut c_void)?;
        if reqbuf.count == 0 {
            return Err(anyhow::anyhow!("{} allocated no capture buffers", device_path));
        }

        // The driver may grant fewer buffers than requested; honour what it says.
        let mut buffers: Vec<MappedBuffer> = Vec::with_capacity(reqbuf.count as usize);
        for index in 0..reqbuf.count {
            let mut buf = v4l2_buffer {
                index,
                type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
                memory: V4L2_MEMORY_MMAP,
                ..unsafe { std::mem::zeroed() }
            };
            ioctl(fd.as_raw_fd(), VIDIOC_QUERYBUF, &mut buf as *mut _ as *mut c_void)?;

            let length = buf.length as usize;
            let offset = buf.m as libc::off_t;
            let ptr = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    length,
                    PROT_READ,
                    MAP_SHARED,
                    fd.as_raw_fd(),
                    offset,
                )
            };
            if ptr == MAP_FAILED {
                // Unmap whatever succeeded before giving up.
                for b in &buffers {
                    unsafe { munmap(b.ptr, b.length) };
                }
                return Err(anyhow::anyhow!("mmap failed for buffer {index}"));
            }
            buffers.push(MappedBuffer { ptr, length });
        }

        Ok(Self { fd, buffers, width, height, stream_on: false, restarts: 0, roi_set: false })
    }

    /// Stream restarts caused by flat-fill frames since the camera was opened.
    pub fn stream_restarts(&self) -> u32 {
        self.restarts
    }

    /// Stop streaming until the next capture.
    ///
    /// Call this before sleeping between captures. A stream left running with
    /// nobody collecting frames lets the reference sensor blank in the dark
    /// and sit there; its auto-exposure then drifts to maximum, and the next
    /// segment starts with the face blown out (lit-frame means of 150–230
    /// against about 35 when metered on the face). The next capture restarts
    /// the stream, which costs about 250 ms.
    pub fn pause(&mut self) {
        self.stop_stream();
    }

    /// Make the sensor's auto-exposure meter on this rectangle of the frame.
    ///
    /// Uses the standard UVC 1.5 region-of-interest control. Windows Hello
    /// does the same, moving the region onto the detected face several times
    /// a second; without it the reference sensor exposes for the dark room and
    /// blows the face out. The rectangle is clamped to the frame. Sensors
    /// without the control return an error, which callers may ignore.
    pub fn set_exposure_region(&mut self, left: u32, top: u32, width: u32, height: u32) -> Result<()> {
        let left = left.min(self.width.saturating_sub(1));
        let top = top.min(self.height.saturating_sub(1));
        let width = width.clamp(1, self.width - left);
        let height = height.clamp(1, self.height - top);
        let mut rect = v4l2_rect { left: left as i32, top: top as i32, width, height };
        let mut ctrls = [
            v4l2_ext_control {
                id: V4L2_CID_UVC_REGION_OF_INTEREST_RECT,
                size: std::mem::size_of::<v4l2_rect>() as u32,
                reserved2: 0,
                value_or_ptr: &mut rect as *mut v4l2_rect as u64,
            },
            v4l2_ext_control {
                id: V4L2_CID_UVC_REGION_OF_INTEREST_AUTO,
                size: 0,
                reserved2: 0,
                value_or_ptr: V4L2_UVC_REGION_OF_INTEREST_AUTO_EXPOSURE as u64,
            },
        ];
        let mut arg = v4l2_ext_controls {
            which: 0, // V4L2_CTRL_WHICH_CUR_VAL
            count: ctrls.len() as u32,
            error_idx: 0,
            request_fd: 0,
            reserved: [0],
            controls: ctrls.as_mut_ptr(),
        };
        ioctl(self.fd.as_raw_fd(), VIDIOC_S_EXT_CTRLS, &mut arg as *mut _ as *mut c_void)?;
        self.roi_set = true;
        Ok(())
    }

    /// Meter exposure on a face box given in frame pixels as `[x1, y1, x2, y2]`.
    pub fn meter_on_face(&mut self, bbox: [f32; 4]) {
        let [x1, y1, x2, y2] = bbox.map(|v| v.max(0.0) as u32);
        if x2 <= x1 || y2 <= y1 {
            return;
        }
        if let Err(e) = self.set_exposure_region(x1, y1, x2 - x1, y2 - y1) {
            tracing::debug!("could not set exposure region: {e}");
        }
    }

    /// Before the first face is found, meter on where one usually is: the
    /// middle half of the width and the lower three quarters of the height.
    fn meter_on_centre(&mut self) {
        let (w, h) = (self.width, self.height);
        if let Err(e) = self.set_exposure_region(w / 4, h / 5, w / 2, h * 3 / 4) {
            tracing::debug!("sensor has no settable exposure region: {e}");
            self.roi_set = true; // don't retry on every stream start
        }
    }

    /// Capture the next frame, restarting the stream if the sensor has blanked.
    ///
    /// In a dark room (no ambient IR, so the unlit strobe frame is completely
    /// black) the reference ASUS sensor (3277:0060) streams about 12 good
    /// frames, then replaces every frame with a solid fill of value 144. The
    /// firmware does this on its own: Windows' inbox driver sends no command
    /// that prevents it, and Windows Hello fails in the dark on the same
    /// hardware. Each new stream start gives another ~12 good frames, so a
    /// flat-fill frame triggers STREAMOFF/STREAMON and a fresh capture.
    pub fn capture_frame(&mut self, timeout_ms: i32) -> Result<IrFrame> {
        let mut restarts_here = 0;
        loop {
            let frame = self.capture_raw_frame(timeout_ms)?;
            if !frame.is_flat_fill() {
                return Ok(frame);
            }
            if restarts_here == MAX_STREAM_RESTARTS {
                return Err(anyhow::anyhow!(
                    "camera sent only flat-fill frames after {restarts_here} stream restarts"
                ));
            }
            restarts_here += 1;
            self.restarts += 1;
            tracing::debug!(total = self.restarts, "flat-fill frame (sensor blanked); restarting stream");
            self.stop_stream();
        }
    }

    fn capture_raw_frame(&mut self, timeout_ms: i32) -> Result<IrFrame> {
        if !self.stream_on {
            if !self.roi_set {
                self.meter_on_centre();
            }
            // Queue every buffer before streaming, so the driver always has
            // somewhere to write and never has to drop a frame.
            for index in 0..self.buffers.len() as u32 {
                let buf = v4l2_buffer {
                    index,
                    type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
                    memory: V4L2_MEMORY_MMAP,
                    ..unsafe { std::mem::zeroed() }
                };
                ioctl(self.fd.as_raw_fd(), VIDIOC_QBUF, &buf as *const _ as *mut c_void)?;
            }
            let stream_type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            ioctl(self.fd.as_raw_fd(), VIDIOC_STREAMON, &stream_type as *const _ as *mut c_void)?;
            self.stream_on = true;
        }

        // Use poll() to wait for data with the configured timeout
        let mut pfd = pollfd {
            fd: self.fd.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        };
        let poll_ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if poll_ret < 0 {
            return Err(anyhow::anyhow!("poll failed: {}", std::io::Error::last_os_error()));
        }
        if poll_ret == 0 {
            return Err(anyhow::anyhow!("Capture timed out after {}ms", timeout_ms));
        }

        let mut buf = v4l2_buffer {
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            ..unsafe { std::mem::zeroed() }
        };
        if ioctl(self.fd.as_raw_fd(), VIDIOC_DQBUF, &mut buf as *mut _ as *mut c_void).is_err() {
            let err = std::io::Error::last_os_error();
            return Err(anyhow::anyhow!("Failed to capture frame: {}", err));
        }

        let index = buf.index as usize;
        let Some(mapped) = self.buffers.get(index) else {
            return Err(anyhow::anyhow!(
                "driver dequeued buffer index {index}, only {} are mapped",
                self.buffers.len()
            ));
        };

        // `bytesused` comes from the driver and is not trusted to fit the
        // mapping: this is the one place external data sizes an unsafe read,
        // and an oversized value would run off the end of the mmap.
        let bytes_used = (buf.bytesused as usize).min(mapped.length);
        if bytes_used < buf.bytesused as usize {
            tracing::warn!(
                reported = buf.bytesused,
                mapped = mapped.length,
                "driver reported more bytes than the buffer holds; truncating"
            );
        }
        let data_slice = unsafe { std::slice::from_raw_parts(mapped.ptr as *const u8, bytes_used) };
        let data: Vec<u16> = data_slice.iter().map(|&b| (b as u16) * 257).collect();

        // Hand the buffer straight back so the ring stays full.
        let _ = ioctl(self.fd.as_raw_fd(), VIDIOC_QBUF, &mut buf as *mut _ as *mut c_void);

        let expected = self.width as usize * self.height as usize;
        if data.len() < expected {
            return Err(anyhow::anyhow!(
                "short frame: got {} bytes, expected {} for {}x{} GREY",
                data.len(),
                expected,
                self.width,
                self.height
            ));
        }

        Ok(IrFrame { data, width: self.width, height: self.height })
    }

    /// Capture a frame, preferring an illuminated one.
    ///
    /// Windows Hello IR modules commonly strobe their illuminator, emitting a
    /// lit frame and a near-black ambient frame alternately. On the reference
    /// ASUS sensor the lit frames average 48–96 (of 255) and the dark ones
    /// 2–8, strictly alternating at 15 fps.
    ///
    /// A dark frame is not merely useless: histogram equalisation stretches its
    /// 0–23 range across the full scale and turns sensor noise into a
    /// high-contrast grey field, which the detector then searches in vain.
    /// Worse, a capture interval that happens to be an even number of frames
    /// locks onto one phase, so an unlucky caller sees *only* dark frames.
    ///
    /// Taking the brighter of two consecutive frames sidesteps all of that
    /// without assuming the strobe exists: on a camera that does not strobe the
    /// two frames are alike and either will do.
    pub fn capture_illuminated_frame(&mut self, timeout_ms: i32) -> Result<IrFrame> {
        let first = self.capture_frame(timeout_ms)?;

        // If the second capture fails, the first is still a usable answer.
        let second = match self.capture_frame(timeout_ms) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("second frame of pair failed, using the first: {e}");
                return Ok(first);
            }
        };

        let (a, b) = (first.mean_intensity(), second.mean_intensity());
        tracing::trace!(first_mean = a, second_mean = b, "illumination pair");
        Ok(if b > a { second } else { first })
    }

    fn stop_stream(&mut self) {
        if self.stream_on {
            let stream_type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            let _ = ioctl(self.fd.as_raw_fd(), VIDIOC_STREAMOFF, &stream_type as *const _ as *mut c_void);
            self.stream_on = false;
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop_stream();
        for b in &self.buffers {
            unsafe { munmap(b.ptr, b.length) };
        }
    }
}

pub fn capture_ir_frame(device_path: &str, timeout_ms: i32) -> Result<IrFrame> {
    let mut cam = Camera::open(device_path)?;
    let frame = cam.capture_illuminated_frame(timeout_ms)?;
    cam.stop_stream();
    Ok(frame)
}

fn ioctl(fd: i32, request: u64, arg: *mut c_void) -> Result<i32> {
    let ret = unsafe { libc::ioctl(fd, request as libc::Ioctl, arg) };
    if ret < 0 {
        Err(anyhow::anyhow!("ioctl failed: {}", std::io::Error::last_os_error()))
    } else {
        Ok(ret)
    }
}

/// Does this V4L2 device name look like an IR sensor?
///
/// Matched on word boundaries rather than as a substring: plain `contains("ir")`
/// also fires on "Virtual Camera" (v4l2loopback) and "BRIO", either of which
/// would be picked as the authentication camera ahead of the real IR sensor.
pub fn name_suggests_ir(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "ir" || word == "infrared")
}

/// One V4L2 node and what was learned about it.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoNode {
    pub path: String,
    /// Card name from sysfs. The kernel truncates this to 31 characters, so
    /// an "IR" suffix is often cut off: the reference ASUS sensor reports
    /// "USB2.0 FHD UVC WebCam: USB2.0 I".
    pub name: String,
    pub ir_name: bool,
    /// `Ok(())` if the node opened as a GREY capture device, else why not.
    pub grey_capture: Result<(), String>,
}

/// Every `/dev/videoN` node, in numeric order, with its sysfs name and
/// whether it opens as a GREY capture device.
pub fn list_video_nodes() -> Vec<VideoNode> {
    let base = std::path::Path::new("/sys/class/video4linux");
    let mut nodes: Vec<VideoNode> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let Some(device_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let name = std::fs::read_to_string(entry.path().join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let path = format!("/dev/{device_name}");
            let grey_capture = Camera::open(&path).map(|_| ()).map_err(|e| e.to_string());
            nodes.push(VideoNode { ir_name: name_suggests_ir(&name), path, name, grey_capture });
        }
    }
    // Sort numerically: /dev/video9 must order before /dev/video10.
    nodes.sort_by_key(|n| {
        (
            n.path.trim_start_matches("/dev/video").parse::<u32>().unwrap_or(u32::MAX),
            n.path.clone(),
        )
    });
    nodes
}

/// Enumerate IR capture devices, best candidate first, as `(path, name)`.
///
/// A node qualifies if it opens as a raw 8-bit GREY capture device; that is
/// what Windows Hello IR sensors expose and what RGB webcams (YUYV, MJPG) do
/// not. Nodes whose sysfs name also says IR are preferred, but the name is
/// not required: the kernel truncates card names to 31 characters and on the
/// reference ASUS laptop the IR sensor's name ends in "USB2.0 I", so a
/// name-only rule found nothing and authentication fell back to the RGB
/// webcam.
///
/// UVC cameras commonly expose several nodes under one name — typically a
/// capture node followed by a metadata node that accepts no frames — which
/// is why each candidate is opened rather than trusted.
pub fn enumerate_ir_cameras() -> Vec<(String, String)> {
    let mut nodes: Vec<VideoNode> = list_video_nodes()
        .into_iter()
        .filter(|n| n.grey_capture.is_ok())
        .collect();
    // Stable sort: IR-named first, numeric order preserved within each group.
    nodes.sort_by_key(|n| !n.ir_name);
    nodes.into_iter().map(|n| (n.path, n.name)).collect()
}

pub fn detect_ir_camera() -> Option<String> {
    enumerate_ir_cameras()
        .into_iter()
        .next()
        .map(|(path, _)| path)
}

/// Is `path` a real IR capture device on this machine?
///
/// Lets an unprivileged setting name *which* IR sensor to use without letting
/// it name an arbitrary video source: the device must be a `/dev/videoN` node
/// registered under `/sys/class/video4linux` and open as a GREY capture
/// device. An ordinary RGB webcam fails the format check. The sysfs name is
/// not consulted, because the kernel truncates it and the reference sensor's
/// "IR" is cut off; selecting among the GREY sensors physically present is a
/// preference, and there is nothing else a GREY node could be short of a
/// loopback device that only root can create.
pub fn is_ir_capture_device(path: &str) -> bool {
    let Some(node) = path.strip_prefix("/dev/") else {
        return false;
    };
    if node.is_empty() || node.contains('/') || node.contains("..") {
        return false;
    }
    if !std::path::Path::new("/sys/class/video4linux").join(node).join("name").exists() {
        return false;
    }

    // Camera::open enforces the GREY pixel format and sane geometry.
    Camera::open(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: Vec<u16>) -> IrFrame {
        IrFrame { width: data.len() as u32, height: 1, data }
    }

    #[test]
    fn flat_fill_is_detected() {
        assert!(frame(vec![144 * 257; 640 * 400]).is_flat_fill());
        assert!(frame(vec![0; 16]).is_flat_fill());
    }

    #[test]
    fn real_frames_are_not_flat() {
        // A near-black unlit frame still has a few distinct values.
        let mut dark = vec![0u16; 640 * 400];
        dark[1234] = 257;
        assert!(!frame(dark).is_flat_fill());
        assert!(!frame(vec![]).is_flat_fill());
    }

    #[test]
    fn recognises_ir_camera_names() {
        assert!(name_suggests_ir("Integrated IR Camera"));
        assert!(name_suggests_ir("Chicony USB2.0 Camera: Infrared"));
        assert!(name_suggests_ir("ir-camera"));
        assert!(name_suggests_ir("IR"));
    }

    #[test]
    fn does_not_match_ir_inside_unrelated_words() {
        // These are the cases plain substring matching got wrong.
        assert!(!name_suggests_ir("Virtual Camera"));
        assert!(!name_suggests_ir("Logitech BRIO"));
        assert!(!name_suggests_ir("Integrated Webcam"));
        assert!(!name_suggests_ir("Mirror Cam"));
    }
}

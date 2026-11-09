//! Decode the shipped glyphs at their native sizes into alpha-channel HICONs.

use super::wide;
use crate::core::engine::Profile;
use std::io::Cursor;
use std::ptr::null_mut;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO,
};

const SIZES: [i32; 6] = [16, 20, 24, 32, 48, 64];
macro_rules! assets {
    ($glyph:literal, $colour:literal) => {
        [
            include_bytes!(concat!(
                "../../assets/tray/",
                $glyph,
                "-",
                $colour,
                "-16.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                "../../assets/tray/",
                $glyph,
                "-",
                $colour,
                "-20.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                "../../assets/tray/",
                $glyph,
                "-",
                $colour,
                "-24.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                "../../assets/tray/",
                $glyph,
                "-",
                $colour,
                "-32.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                "../../assets/tray/",
                $glyph,
                "-",
                $colour,
                "-48.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                "../../assets/tray/",
                $glyph,
                "-",
                $colour,
                "-64.png"
            ))
            .as_slice(),
        ]
    };
}
const ASSETS: [[[&[u8]; 6]; 2]; 3] = [
    [assets!("bolt", "black"), assets!("bolt", "white")],
    [assets!("yin-yang", "black"), assets!("yin-yang", "white")],
    [assets!("recycle", "black"), assets!("recycle", "white")],
];

fn glyph(profile: Profile) -> usize {
    match profile {
        Profile::Performance => 0,
        Profile::Balanced | Profile::Unknown => 1,
        Profile::PowerSaver => 2,
    }
}

fn nearest_size(size: i32) -> usize {
    SIZES
        .iter()
        .enumerate()
        .min_by_key(|(_, candidate)| (**candidate - size).abs())
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn taskbar_is_dark() -> bool {
    let mut light = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the strings and DWORD output are valid for the synchronous query.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            wide("SystemUsesLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut light as *mut u32).cast(),
            &mut size,
        )
    };
    status != 0 || light == 0
}

fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let mut pixels = vec![0; reader.output_buffer_size().ok_or("icon PNG is too large")?];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|error| error.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("tray icons must be 8-bit RGBA PNGs".into());
    }
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}

/// Windows' alpha icons use premultiplied BGRA in a top-down colour DIB.
fn premultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u16;
        let red = ((pixel[0] as u16 * alpha + 127) / 255) as u8;
        pixel[0] = ((pixel[2] as u16 * alpha + 127) / 255) as u8;
        pixel[1] = ((pixel[1] as u16 * alpha + 127) / 255) as u8;
        pixel[2] = red;
    }
}

pub struct Icon(HICON);

impl Icon {
    pub fn new(profile: Profile, requested_size: i32) -> Result<Self, String> {
        let bytes =
            ASSETS[glyph(profile)][usize::from(taskbar_is_dark())][nearest_size(requested_size)];
        let (width, height, mut pixels) = decode(bytes)?;
        premultiply(&mut pixels);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = null_mut();
        // SAFETY: info describes the exact pixel allocation and bits is writable.
        let colour = unsafe {
            CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0)
        };
        if colour.is_null() {
            return Err(format!(
                "create tray bitmap: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: the 32-bit DIB has width*height*4 bytes, matching the decoded RGBA.
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast(), pixels.len()) };
        let mask_bits = vec![0u8; ((width as usize).div_ceil(16) * 2) * height as usize];
        // SAFETY: the monochrome mask has word-aligned rows and enough initialized bytes.
        let mask =
            unsafe { CreateBitmap(width as i32, height as i32, 1, 1, mask_bits.as_ptr().cast()) };
        if mask.is_null() {
            // SAFETY: colour is our unshared GDI bitmap.
            unsafe { DeleteObject(colour) };
            return Err(format!(
                "create tray mask: {}",
                std::io::Error::last_os_error()
            ));
        }
        let info = ICONINFO {
            fIcon: 1,
            hbmColor: colour,
            hbmMask: mask,
            ..Default::default()
        };
        // SAFETY: both bitmaps are live; CreateIconIndirect copies their contents.
        let icon = unsafe { CreateIconIndirect(&info) };
        // SAFETY: copied bitmaps are ours and are no longer needed by the HICON.
        unsafe {
            DeleteObject(mask);
            DeleteObject(colour);
        }
        if icon.is_null() {
            Err(format!(
                "create tray icon: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(icon))
        }
    }

    pub fn handle(&self) -> HICON {
        self.0
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: this is our owned, non-shared icon, destroyed exactly once.
        unsafe { DestroyIcon(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_icons_decode_at_their_declared_sizes() {
        for glyph in ASSETS {
            for colour in glyph {
                for (index, bytes) in colour.iter().enumerate() {
                    let (width, height, pixels) = decode(bytes).unwrap();
                    assert_eq!((width, height), (SIZES[index] as u32, SIZES[index] as u32));
                    assert_eq!(pixels.len(), (width * height * 4) as usize);
                }
            }
        }
    }

    #[test]
    fn sizes_glyphs_and_alpha() {
        assert_eq!(nearest_size(19), 1);
        assert_eq!(nearest_size(30), 3);
        assert_eq!(nearest_size(100), 5);
        assert_eq!(glyph(Profile::Unknown), glyph(Profile::Balanced));
        let mut pixels = [255, 100, 0, 128, 1, 2, 3, 0];
        premultiply(&mut pixels);
        assert_eq!(pixels, [0, 50, 128, 128, 0, 0, 0, 0]);
    }
}

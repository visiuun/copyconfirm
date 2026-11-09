use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{GUID, HSTRING, Result};

pub const THUMB_SIZE: u32 = 96;

/// Premultiplied BGRA, top-down.
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct Wic(IWICImagingFactory);

impl Wic {
    pub fn new() -> Result<Wic> {
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map(Wic) }
    }

    pub fn decode_memory(&self, bytes: &[u8]) -> Result<IWICBitmapFrameDecode> {
        unsafe {
            let stream = self.0.CreateStream()?;
            stream.InitializeFromMemory(bytes)?;
            let decoder = self.0.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
            decoder.GetFrame(0)
        }
    }

    pub fn decode_file(&self, path: &str) -> Result<IWICBitmapFrameDecode> {
        unsafe {
            let decoder =
                self.0.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ_ACCESS, WICDecodeMetadataCacheOnDemand)?;
            decoder.GetFrame(0)
        }
    }

    fn convert(&self, source: &IWICBitmapSource, format: &GUID) -> Result<IWICBitmapSource> {
        unsafe {
            let converter = self.0.CreateFormatConverter()?;
            converter.Initialize(source, format, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            windows::core::Interface::cast(&converter)
        }
    }

    pub fn thumbnail(&self, frame: &IWICBitmapFrameDecode) -> Result<Thumb> {
        unsafe {
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h)?;
            let short = w.min(h).max(1) as f64;
            let ratio = THUMB_SIZE as f64 / short;
            let (sw, sh) = (((w as f64 * ratio).round() as u32).max(THUMB_SIZE), ((h as f64 * ratio).round() as u32).max(THUMB_SIZE));
            let scaler = self.0.CreateBitmapScaler()?;
            scaler.Initialize(frame, sw, sh, WICBitmapInterpolationModeFant)?;
            let source: IWICBitmapSource = windows::core::Interface::cast(&scaler)?;
            let converted = self.convert(&source, &GUID_WICPixelFormat32bppPBGRA)?;
            let rect = WICRect {
                X: ((sw - THUMB_SIZE) / 2) as i32,
                Y: ((sh - THUMB_SIZE) / 2) as i32,
                Width: THUMB_SIZE as i32,
                Height: THUMB_SIZE as i32,
            };
            let stride = THUMB_SIZE * 4;
            let mut pixels = vec![0u8; (stride * THUMB_SIZE) as usize];
            converted.CopyPixels(&rect, stride, &mut pixels)?;
            Ok(Thumb { width: THUMB_SIZE, height: THUMB_SIZE, pixels })
        }
    }

    pub fn encode_png(&self, frame: &IWICBitmapFrameDecode, path: &std::path::Path) -> Result<()> {
        unsafe {
            let stream = self.0.CreateStream()?;
            stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
            let encoder = self.0.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut target = None;
            encoder.CreateNewFrame(&mut target, std::ptr::null_mut())?;
            let target = target.ok_or_else(windows::core::Error::empty)?;
            target.Initialize(None)?;
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h)?;
            target.SetSize(w, h)?;
            let mut format = GUID_WICPixelFormat32bppBGRA;
            target.SetPixelFormat(&mut format)?;
            let source: IWICBitmapSource = windows::core::Interface::cast(frame)?;
            let converted = self.convert(&source, &GUID_WICPixelFormat32bppBGRA)?;
            target.WriteSource(&converted, std::ptr::null())?;
            target.Commit()?;
            encoder.Commit()
        }
    }

    /// Decodes to a CF_DIB payload: BITMAPINFOHEADER plus bottom-up BGRA.
    pub fn dib_from_file(&self, path: &str) -> Result<Vec<u8>> {
        unsafe {
            let frame = self.decode_file(path)?;
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h)?;
            let source: IWICBitmapSource = windows::core::Interface::cast(&frame)?;
            let converted = self.convert(&source, &GUID_WICPixelFormat32bppBGRA)?;
            let stride = w as usize * 4;
            let size = stride * h as usize;
            let mut top_down = vec![0u8; size];
            converted.CopyPixels(std::ptr::null(), stride as u32, &mut top_down)?;
            let mut dib = Vec::with_capacity(40 + size);
            dib.extend(40u32.to_le_bytes());
            dib.extend((w as i32).to_le_bytes());
            dib.extend((h as i32).to_le_bytes());
            dib.extend(1u16.to_le_bytes());
            dib.extend(32u16.to_le_bytes());
            dib.extend(0u32.to_le_bytes());
            dib.extend((size as u32).to_le_bytes());
            dib.extend([0u8; 16]);
            for row in top_down.rchunks_exact(stride.max(1)) {
                dib.extend_from_slice(row);
            }
            Ok(dib)
        }
    }
}

const GENERIC_READ_ACCESS: windows::Win32::Foundation::GENERIC_ACCESS_RIGHTS = windows::Win32::Foundation::GENERIC_READ;

pub fn frame_size(frame: &IWICBitmapFrameDecode) -> (u32, u32) {
    let (mut w, mut h) = (0, 0);
    unsafe {
        let _ = frame.GetSize(&mut w, &mut h);
    }
    (w, h)
}

/// Prepends the BITMAPFILEHEADER a CF_DIB lacks. Returns `None` for a malformed header.
pub fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes(dib[i..i + 4].try_into().unwrap());
    let header = u32_at(0) as u64;
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().unwrap()) as u32;
    let compression = u32_at(16);
    let mut colors = u32_at(32) as u64;
    if colors == 0 && bit_count <= 8 {
        colors = 1 << bit_count;
    }
    if !(40..=124).contains(&header) || colors > 256 {
        return None;
    }
    // BI_BITFIELDS with a plain 40-byte header keeps three masks after it.
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let offset = 14 + header + masks + colors * 4;
    if offset >= dib.len() as u64 + 14 {
        return None;
    }
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend(b"BM");
    bmp.extend(((14 + dib.len()) as u32).to_le_bytes());
    bmp.extend([0u8; 4]);
    bmp.extend((offset as u32).to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

pub fn hash(bytes: &[u8]) -> u64 {
    const PRIME: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = (bytes.len() as u64).wrapping_mul(PRIME);
    let (chunks, rest) = bytes.as_chunks::<8>();
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        h = (h ^ word).wrapping_mul(PRIME).rotate_left(29);
    }
    for byte in rest {
        h = (h ^ *byte as u64).wrapping_mul(PRIME).rotate_left(11);
    }
    h ^ (h >> 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_offsets() {
        let mut dib = vec![0u8; 64];
        dib[0] = 40;
        dib[14] = 32;
        assert_eq!(u32::from_le_bytes(bmp_from_dib(&dib).unwrap()[10..14].try_into().unwrap()), 54);
        dib[16] = 3;
        assert_eq!(u32::from_le_bytes(bmp_from_dib(&dib).unwrap()[10..14].try_into().unwrap()), 66);
    }

    #[test]
    fn dib_rejects_nonsense() {
        assert!(bmp_from_dib(&[0; 8]).is_none());
        let mut dib = vec![0u8; 64];
        dib[0] = 7;
        assert!(bmp_from_dib(&dib).is_none());
    }

    #[test]
    fn hash_sees_single_byte_changes() {
        let a = vec![7u8; 4096];
        let mut b = a.clone();
        b[2049] = 8;
        assert_ne!(hash(&a), hash(&b));
    }
}

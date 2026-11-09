//! Writes the icon and manifest as a .res file for the MSVC linker, so rc.exe isn't required.

use std::io::Write;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_MANIFEST: u16 = 24;
const ICON_SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 256];

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="CopyConfirm" version="1.0.0.0"/>
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1"><application>
    <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
  </application></compatibility>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges>
    <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
  </requestedPrivileges></security></trustInfo>
  <asmv3:application><asmv3:windowsSettings>
    <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
    <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
  </asmv3:windowsSettings></asmv3:application>
</assembly>
"#;

fn segment_distance(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let t = (((px - a.0) * vx + (py - a.1) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    ((px - a.0 - vx * t).powi(2) + (py - a.1 - vy * t).powi(2)).sqrt()
}

fn icon_pixel(u: f32, v: f32) -> [f32; 2] {
    let radius = 0.24;
    let (qx, qy) = ((u - 0.5).abs() - (0.5 - radius), (v - 0.5).abs() - (0.5 - radius));
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
    if outside > 0.0 {
        return [0.0, 0.0];
    }
    let check = segment_distance(u, v, (0.26, 0.53), (0.43, 0.70)).min(segment_distance(u, v, (0.43, 0.70), (0.75, 0.33)));
    [if check < 0.062 { 0.0 } else { 1.0 }, 1.0]
}

fn icon_dib(size: u32) -> Vec<u8> {
    const SS: u32 = 5;
    let mask_row = size.div_ceil(32) * 4;
    let mut v = Vec::new();
    v.extend(40u32.to_le_bytes());
    v.extend((size as i32).to_le_bytes());
    v.extend((size as i32 * 2).to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(32u16.to_le_bytes());
    v.extend(0u32.to_le_bytes());
    v.extend((size * size * 4 + mask_row * size).to_le_bytes());
    v.extend([0u8; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let (mut lum, mut alpha) = (0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let w = (y as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let [l, a] = icon_pixel(u, w);
                    lum += l * a;
                    alpha += a;
                }
            }
            let n = (SS * SS) as f32;
            let a = alpha / n;
            let c = if alpha > 0.0 { lum / alpha } else { 0.0 };
            let byte = (c * 255.0 + 0.5) as u8;
            v.extend([byte, byte, byte, (a * 255.0 + 0.5) as u8]);
        }
    }
    v.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    v
}

fn res_entry(out: &mut Vec<u8>, ty: u16, id: u16, flags: u16, lang: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(ty.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(lang.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let mut res = Vec::new();
    res_entry(&mut res, 0, 0, 0, 0, &[]);

    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((ICON_SIZES.len() as u16).to_le_bytes());
    for (i, &size) in ICON_SIZES.iter().enumerate() {
        let dib = icon_dib(size);
        let id = i as u16 + 1;
        res_entry(&mut res, RT_ICON, id, 0x1010, 0x0409, &dib);
        let dim = if size >= 256 { 0 } else { size as u8 };
        group.extend([dim, dim, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((dib.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    res_entry(&mut res, RT_GROUP_ICON, 1, 0x1030, 0x0409, &group);
    res_entry(&mut res, RT_MANIFEST, 1, 0x1030, 0x0409, MANIFEST.as_bytes());

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("copyconfirm.res");
    std::fs::File::create(&out).unwrap().write_all(&res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

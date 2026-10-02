//! Downloads a guest asks for: its own data (`api_download_data`), a URL (`api_download_url`)
//! and the canvas as a PDF (`api_canvas_print_pdf`). The engine writes no file itself: each is
//! queued in the document's [`HostState::downloads`] and, once the guest's call has returned,
//! handed to the browser as a download request (`Service::Download`, by the view), which the
//! core allows or refuses (one a second per tab) and the browser performs: its download folder,
//! a file name it cleans from the guest's, never one that leads elsewhere. A worker's guest has
//! no view to ask through, so its downloads fail.

use std::fmt::Write as _;

use anyhow::Result;
use sighurt_api::services::DownloadSource;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_bytes, guest_str, CanvasState, ConsoleLevel, DrawCommand, HostState, Rgba, MAX_URL,
};

/// The most downloads a document may have waiting for the browser; more fail.
pub const MAX_QUEUED_DOWNLOADS: usize = 4;

/// The most bytes of the guest's own data its waiting downloads may hold; more fail.
pub const MAX_QUEUED_DOWNLOAD_BYTES: usize = 64 << 20;

/// A download a guest asked for, until its view hands it to the browser.
#[derive(Debug)]
pub struct Download {
    /// What to save.
    pub source: DownloadSource,
    /// The file name the guest proposed, which the browser cleans or replaces.
    pub name: Option<String>,
}

/// Register the download host functions.
pub fn register_download_functions(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "oxide",
        "api_download_data",
        |caller: Caller<'_, HostState>,
         data_ptr: u32,
         data_len: u32,
         filename_ptr: u32,
         filename_len: u32|
         -> i32 {
            let filename = guest_str(&caller, filename_ptr, filename_len).unwrap_or_default();
            if data_len == 0 || data_len as usize > MAX_QUEUED_DOWNLOAD_BYTES || filename.is_empty()
            {
                return -1;
            }
            let Some(data) = guest_bytes(&caller, data_ptr, data_len) else {
                return -1;
            };
            let source = DownloadSource::Data(data);
            queue(caller.data(), source, Some(filename), "[DOWNLOAD]")
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_download_url",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> i32 {
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            if url.is_empty() || url.len() > MAX_URL {
                return -1;
            }
            queue(caller.data(), DownloadSource::Url(url), None, "[DOWNLOAD]")
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_print_pdf",
        |caller: Caller<'_, HostState>, filename_ptr: u32, filename_len: u32| -> i32 {
            let filename = guest_str(&caller, filename_ptr, filename_len).unwrap_or_default();
            if filename.is_empty() {
                return -1;
            }
            let pdf = canvas_pdf(&caller.data().canvas.lock().unwrap());
            let name = if filename.trim().is_empty() {
                format!(
                    "{}",
                    chrono::Local::now().format("sighurt-canvas-%Y%m%d-%H%M%S.pdf")
                )
            } else if filename.to_lowercase().ends_with(".pdf") {
                filename
            } else {
                format!("{filename}.pdf")
            };
            queue(
                caller.data(),
                DownloadSource::Data(pdf),
                Some(name),
                "[PRINT]",
            )
        },
    )?;

    Ok(())
}

/// Queues a download of `source` for the browser, as `name`, and logs it with `tag`: 0 if it
/// was queued, -1 if not (in a worker, or beyond the queue's bounds).
fn queue(state: &HostState, source: DownloadSource, name: Option<String>, tag: &str) -> i32 {
    let what = match &source {
        DownloadSource::Url(url) => url.clone(),
        DownloadSource::Data(data) => format!("{} bytes", data.len()),
    };
    let refused = if state.worker_outbox.is_some() {
        Some("a worker can't download")
    } else {
        let mut queued = state.downloads.lock().unwrap();
        let bytes = |download: &Download| match &download.source {
            DownloadSource::Data(data) => data.len(),
            DownloadSource::Url(_) => 0,
        };
        let new = Download { source, name };
        let total = queued.iter().map(bytes).sum::<usize>() + bytes(&new);
        if queued.len() >= MAX_QUEUED_DOWNLOADS || total > MAX_QUEUED_DOWNLOAD_BYTES {
            Some("too many downloads at once")
        } else {
            queued.push(new);
            None
        }
    };
    match refused {
        None => {
            state.log(
                ConsoleLevel::Log,
                format!("{tag} {what}: asked the browser to save it"),
            );
            0
        }
        Some(why) => {
            state.log(
                ConsoleLevel::Error,
                format!("{tag} {what}: not saved: {why}"),
            );
            -1
        }
    }
}

/// Points per canvas pixel (96 dpi).
const PT_PER_PX: f32 = 0.75;

/// A one-page PDF of the canvas using the standard Helvetica font (nothing embedded). It has
/// the rectangles, text in the default font, lines, circles, arcs, curves and rounded
/// rectangles; images, gradients, styled text and transforms are left out.
fn canvas_pdf(canvas: &CanvasState) -> Vec<u8> {
    let (width, height) = (
        canvas.width as f32 * PT_PER_PX,
        canvas.height as f32 * PT_PER_PX,
    );
    // A canvas point in PDF coordinates, which have y pointing up.
    let pt = |x: f32, y: f32| format!("{:.1} {:.1}", x * PT_PER_PX, height - y * PT_PER_PX);
    let rgb = |[r, g, b, _]: Rgba| {
        let c = |v: u8| f32::from(v) / 255.0;
        format!("{:.3} {:.3} {:.3}", c(r), c(g), c(b))
    };
    let mut content = String::new();
    for command in &canvas.commands {
        let _ = match *command {
            DrawCommand::Clear { color } => {
                writeln!(content, "{} rg 0 0 {width:.1} {height:.1} re f", rgb(color))
            }
            DrawCommand::Rect { x, y, w, h, color } => writeln!(
                content,
                "{} rg {} {:.1} {:.1} re f",
                rgb(color),
                pt(x, y + h),
                w * PT_PER_PX,
                h * PT_PER_PX
            ),
            DrawCommand::Text {
                x,
                y,
                size,
                color,
                ref text,
            } => writeln!(
                content,
                "BT {} rg /F1 {:.1} Tf {} Td ({}) Tj ET",
                rgb(color),
                size * PT_PER_PX,
                pt(x, y),
                escape_pdf_string(text)
            ),
            DrawCommand::Line {
                x1,
                y1,
                x2,
                y2,
                color,
                thickness,
            } => writeln!(
                content,
                "{} RG {:.1} w {} m {} l S",
                rgb(color),
                thickness * PT_PER_PX,
                pt(x1, y1),
                pt(x2, y2)
            ),
            DrawCommand::Circle {
                cx,
                cy,
                radius,
                color,
            } => {
                let d = 2.0 * radius;
                let path = rounded_rect(cx - radius, cy - radius, d, d, radius, height);
                writeln!(content, "{} rg {path} f", rgb(color))
            }
            DrawCommand::RoundedRect {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => {
                let path = rounded_rect(x, y, w, h, radius, height);
                writeln!(content, "{} rg {path} f", rgb(color))
            }
            DrawCommand::Arc {
                cx,
                cy,
                radius,
                start_angle,
                end_angle,
                color,
                thickness,
            } => {
                let sweep = if end_angle > start_angle {
                    end_angle - start_angle
                } else {
                    end_angle + std::f32::consts::TAU - start_angle
                };
                let points: Vec<String> = (0..=32)
                    .map(|i| {
                        let a = start_angle + sweep * i as f32 / 32.0;
                        pt(cx + radius * a.cos(), cy + radius * a.sin())
                    })
                    .collect();
                writeln!(
                    content,
                    "{} RG {:.1} w {} m {} l S",
                    rgb(color),
                    thickness * PT_PER_PX,
                    points[0],
                    points[1..].join(" l ")
                )
            }
            DrawCommand::Bezier {
                x1,
                y1,
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x2,
                y2,
                color,
                thickness,
            } => writeln!(
                content,
                "{} RG {:.1} w {} m {} {} {} c S",
                rgb(color),
                thickness * PT_PER_PX,
                pt(x1, y1),
                pt(cp1x, cp1y),
                pt(cp2x, cp2y),
                pt(x2, y2)
            ),
            DrawCommand::Image { .. }
            | DrawCommand::Gradient { .. }
            | DrawCommand::TextEx { .. }
            | DrawCommand::Save
            | DrawCommand::Restore
            | DrawCommand::Transform { .. }
            | DrawCommand::Clip { .. }
            | DrawCommand::Opacity { .. } => Ok(()),
        };
    }

    let objects = [
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {width:.1} {height:.1}]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>"
        ),
        format!(
            "<</Length {}>>stream\n{content}\nendstream\n",
            content.len()
        ),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
    ];
    let mut pdf = String::new();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        let _ = writeln!(pdf, "{} 0 obj{object}endobj", i + 1);
    }
    let xref = pdf.len();
    let _ = write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(pdf, "{offset:010} 00000 n ");
    }
    let _ = write!(
        pdf,
        "trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    pdf.into_bytes()
}

/// The PDF path of a rectangle with corners rounded by `radius`, from canvas coordinates.
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, page_height: f32) -> String {
    let (x0, x1) = (x * PT_PER_PX, (x + w) * PT_PER_PX);
    let (y0, y1) = (
        page_height - (y + h) * PT_PER_PX,
        page_height - y * PT_PER_PX,
    );
    let r = (radius * PT_PER_PX)
        .min((x1 - x0) / 2.0)
        .min((y1 - y0) / 2.0);
    // Distance of each corner's Bézier control points from the corner.
    let k = r * (1.0 - 0.552_284_8);
    let p = |x: f32, y: f32| format!("{x:.1} {y:.1}");
    format!(
        "{} m {} l {} {} {} c {} l {} {} {} c {} l {} {} {} c {} l {} {} {} c",
        p(x0 + r, y0),
        p(x1 - r, y0),
        p(x1 - k, y0),
        p(x1, y0 + k),
        p(x1, y0 + r),
        p(x1, y1 - r),
        p(x1, y1 - k),
        p(x1 - k, y1),
        p(x1 - r, y1),
        p(x0 + r, y1),
        p(x0 + k, y1),
        p(x0, y1 - k),
        p(x0, y1 - r),
        p(x0, y0 + r),
        p(x0, y0 + k),
        p(x0 + k, y0),
        p(x0 + r, y0),
    )
}

fn escape_pdf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii() => out.push(c),
            _ => {} // Skip non-ASCII — standard PDF fonts only support Latin-1
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pdf_is_well_formed() {
        let mut canvas = CanvasState::default();
        canvas.push(DrawCommand::Clear {
            color: [255, 0, 0, 255],
        });
        canvas.push(DrawCommand::Text {
            x: 10.0,
            y: 20.0,
            size: 16.0,
            color: [0, 0, 0, 255],
            text: "Hi (there)".into(),
        });
        canvas.push(DrawCommand::Circle {
            cx: 50.0,
            cy: 50.0,
            radius: 10.0,
            color: [0, 0, 255, 255],
        });
        let pdf = String::from_utf8(canvas_pdf(&canvas)).unwrap();
        assert!(pdf.contains("1.000 0.000 0.000 rg 0 0 600.0 450.0 re f"));
        assert!(pdf.contains("(Hi \\(there\\)) Tj"));
        // Every cross-reference entry points at its object.
        let xref = pdf.find("\nxref\n").unwrap() + 1;
        let entries: Vec<&str> = pdf[xref..].lines().skip(3).take(5).collect();
        assert_eq!(entries.len(), 5);
        for (i, line) in entries.into_iter().enumerate() {
            let offset: usize = line[..10].parse().unwrap();
            assert!(
                pdf[offset..].starts_with(&format!("{} 0 obj", i + 1)),
                "{line}"
            );
        }
        let startxref: usize = pdf.lines().rev().nth(1).unwrap().parse().unwrap();
        assert_eq!(startxref, xref);
    }
}

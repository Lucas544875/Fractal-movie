//! Contact sheets: labelled grids of thumbnails an agent can look at in one
//! image and refer back to by number.

use std::path::Path;

use anyhow::{Context, Result, bail};

const GLYPH_WIDTH: usize = 5;
const GLYPH_HEIGHT: usize = 7;
/// Pixels per glyph dot. Labels must stay readable after an image model
/// downsamples the sheet.
const GLYPH_SCALE: usize = 3;
const LABEL_PADDING: usize = 4;
const GUTTER: usize = 4;

/// 5x7 dot patterns, one row per byte, most significant of five bits left.
fn glyph(character: char) -> Option<[u8; GLYPH_HEIGHT]> {
    Some(match character {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        _ => return None,
    })
}

/// One thumbnail and the label drawn over it.
pub struct Cell<'a> {
    /// Digits and hyphens only.
    pub label: &'a str,
    /// Tightly packed RGBA8 of the cell size given to [`compose`].
    pub rgba: &'a [u8],
}

/// A composed sheet as tightly packed RGBA8.
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Sheet {
    pub fn save_png(&self, path: &Path) -> Result<()> {
        save_png(path, self.width, self.height, &self.rgba)
    }
}

/// Writes RGBA8 as PNG through a temporary file, so a reader never sees a
/// partial image.
pub fn save_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("png.tmp-{}", std::process::id()));
    image::save_buffer_with_format(
        &temporary,
        rgba,
        width,
        height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .with_context(|| format!("could not encode {}", path.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("could not write {}", path.display()))
}

/// Decodes an image file into a frame for measurement.
pub fn load_frame(path: &Path) -> Result<crate::metrics::Frame> {
    let image = image::open(path)
        .with_context(|| format!("could not decode {}", path.display()))?
        .into_rgba8();
    crate::metrics::Frame::from_rgba8(image.width(), image.height(), image.as_raw())
}

/// Halves an RGBA8 image by averaging 2x2 blocks. Odd trailing rows and
/// columns are dropped.
#[must_use]
pub fn halve(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let mut halved = Vec::with_capacity((width / 2) * (height / 2) * 4);
    for y in 0..height / 2 {
        for x in 0..width / 2 {
            for channel in 0..4 {
                let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .iter()
                    .map(|(dx, dy)| {
                        u32::from(rgba[((y * 2 + dy) * width + x * 2 + dx) * 4 + channel])
                    })
                    .sum();
                halved.push(((sum + 2) / 4) as u8);
            }
        }
    }
    halved
}

/// Fits an RGBA8 image into a cell of another size without distorting it:
/// scaled to fit by nearest-neighbour sampling and centred on black.
#[must_use]
pub fn fit_cell(
    rgba: &[u8],
    width: u32,
    height: u32,
    cell_width: u32,
    cell_height: u32,
) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let (cell_width, cell_height) = (cell_width as usize, cell_height as usize);
    let mut cell = [0u8, 0, 0, 255].repeat(cell_width * cell_height);
    if width == 0 || height == 0 {
        return cell;
    }
    // The largest size with the source's aspect ratio that fits the cell.
    let (fit_width, fit_height) = if width * cell_height > height * cell_width {
        (cell_width, (height * cell_width / width).max(1))
    } else {
        ((width * cell_height / height).max(1), cell_height)
    };
    let (left, top) = ((cell_width - fit_width) / 2, (cell_height - fit_height) / 2);
    for y in 0..fit_height {
        let source_y = y * height / fit_height;
        for x in 0..fit_width {
            let source = (source_y * width + x * width / fit_width) * 4;
            let target = ((top + y) * cell_width + left + x) * 4;
            cell[target..target + 4].copy_from_slice(&rgba[source..source + 4]);
        }
    }
    cell
}

/// Lays cells out row by row in a grid of `columns`.
pub fn compose(
    cells: &[Cell<'_>],
    columns: usize,
    cell_width: usize,
    cell_height: usize,
) -> Result<Sheet> {
    if cells.is_empty() || columns == 0 {
        bail!("a contact sheet needs at least one cell and one column");
    }
    let rows = cells.len().div_ceil(columns);
    let columns = columns.min(cells.len());
    let width = columns * cell_width + (columns + 1) * GUTTER;
    let height = rows * cell_height + (rows + 1) * GUTTER;
    // Mid-grey gutters stay distinct from both dark and bright thumbnails.
    let mut rgba = [96u8, 96, 96, 255].repeat(width * height);

    for (index, cell) in cells.iter().enumerate() {
        if cell.rgba.len() != cell_width * cell_height * 4 {
            bail!(
                "cell {:?} holds {} bytes, expected {cell_width}x{cell_height} RGBA",
                cell.label,
                cell.rgba.len()
            );
        }
        let left = GUTTER + (index % columns) * (cell_width + GUTTER);
        let top = GUTTER + (index / columns) * (cell_height + GUTTER);
        for row in 0..cell_height {
            let source = row * cell_width * 4;
            let target = ((top + row) * width + left) * 4;
            rgba[target..target + cell_width * 4]
                .copy_from_slice(&cell.rgba[source..source + cell_width * 4]);
        }
        draw_label(
            &mut rgba,
            width,
            left,
            top,
            cell_width,
            cell_height,
            cell.label,
        )?;
    }

    Ok(Sheet {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

fn draw_label(
    rgba: &mut [u8],
    sheet_width: usize,
    left: usize,
    top: usize,
    cell_width: usize,
    cell_height: usize,
    label: &str,
) -> Result<()> {
    let advance = (GLYPH_WIDTH + 1) * GLYPH_SCALE;
    let box_width = label.chars().count() * advance + 2 * LABEL_PADDING;
    let box_height = GLYPH_HEIGHT * GLYPH_SCALE + 2 * LABEL_PADDING;
    if box_width > cell_width || box_height > cell_height {
        bail!("label {label:?} does not fit a {cell_width}x{cell_height} cell");
    }
    let mut put = |x: usize, y: usize, value: u8| {
        let offset = ((top + y) * sheet_width + left + x) * 4;
        rgba[offset..offset + 3].fill(value);
    };
    for y in 0..box_height {
        for x in 0..box_width {
            put(x, y, 0);
        }
    }
    for (position, character) in label.chars().enumerate() {
        let Some(rows) = glyph(character) else {
            bail!("label {label:?} contains {character:?}, which has no glyph");
        };
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..GLYPH_WIDTH {
                if bits & (1 << (GLYPH_WIDTH - 1 - column)) == 0 {
                    continue;
                }
                for dy in 0..GLYPH_SCALE {
                    for dx in 0..GLYPH_SCALE {
                        put(
                            LABEL_PADDING + position * advance + column * GLYPH_SCALE + dx,
                            LABEL_PADDING + row * GLYPH_SCALE + dy,
                            255,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (usize, usize) = (120, 60);

    fn solid(value: u8) -> Vec<u8> {
        [value, value, value, 255].repeat(CELL.0 * CELL.1)
    }

    fn pixel(sheet: &Sheet, x: usize, y: usize) -> u8 {
        sheet.rgba[(y * sheet.width as usize + x) * 4]
    }

    #[test]
    fn grid_dimensions_follow_cell_count() {
        let image = solid(200);
        let cells: Vec<Cell<'_>> = (0..5)
            .map(|_| Cell {
                label: "1-2",
                rgba: &image,
            })
            .collect();
        let sheet = compose(&cells, 3, CELL.0, CELL.1).unwrap();
        assert_eq!(sheet.width as usize, 3 * CELL.0 + 4 * GUTTER);
        assert_eq!(sheet.height as usize, 2 * CELL.1 + 3 * GUTTER);
        assert_eq!(sheet.rgba.len(), (sheet.width * sheet.height * 4) as usize);

        // Fewer cells than columns must not leave empty columns.
        let narrow = compose(&cells[..2], 3, CELL.0, CELL.1).unwrap();
        assert_eq!(narrow.width as usize, 2 * CELL.0 + 3 * GUTTER);
    }

    #[test]
    fn cells_are_placed_in_reading_order_with_labels_on_top() {
        let (dark, bright) = (solid(40), solid(220));
        let cells = [
            Cell {
                label: "7",
                rgba: &dark,
            },
            Cell {
                label: "8",
                rgba: &bright,
            },
        ];
        let sheet = compose(&cells, 2, CELL.0, CELL.1).unwrap();
        let second_left = 2 * GUTTER + CELL.0;
        // Bottom-right corners are untouched by labels.
        assert_eq!(pixel(&sheet, GUTTER + CELL.0 - 1, GUTTER + CELL.1 - 1), 40);
        assert_eq!(
            pixel(&sheet, second_left + CELL.0 - 1, GUTTER + CELL.1 - 1),
            220
        );
        // The label box is black with white glyph dots inside it.
        assert_eq!(pixel(&sheet, second_left, GUTTER), 0);
        let box_height = GLYPH_HEIGHT * GLYPH_SCALE + 2 * LABEL_PADDING;
        let lit = (0..box_height)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|(x, y)| pixel(&sheet, second_left + x, GUTTER + y) == 255)
            .count();
        // "8" has 17 dots in the 5x7 pattern above.
        assert_eq!(lit, 17 * GLYPH_SCALE * GLYPH_SCALE);
    }

    #[test]
    fn different_labels_draw_different_glyphs() {
        let image = solid(100);
        let render = |label: &'static str| {
            compose(
                &[Cell {
                    label,
                    rgba: &image,
                }],
                1,
                CELL.0,
                CELL.1,
            )
            .unwrap()
            .rgba
        };
        assert_ne!(render("12-3"), render("12-8"));
        for digit in "0123456789-".chars() {
            assert!(glyph(digit).is_some());
        }
    }

    #[test]
    fn halving_averages_blocks_and_drops_odd_edges() {
        // 3x2 image: a 2x2 block of 0, 100, 200, 100 and an odd column.
        let pixel = |value: u8| [value, value, value, 255];
        let rgba: Vec<u8> = [0, 100, 9, 200, 100, 9]
            .into_iter()
            .flat_map(pixel)
            .collect();
        assert_eq!(halve(&rgba, 3, 2), vec![100, 100, 100, 255]);
        assert!(halve(&rgba, 1, 1).is_empty());
    }

    #[test]
    fn fitting_keeps_the_aspect_ratio_and_centres_the_image() {
        // A 4x2 white image into a 4x4 cell: two black rows, the image, two
        // black rows... here one above and one below.
        let white = [255u8; 4 * 2 * 4];
        let cell = fit_cell(&white, 4, 2, 4, 4);
        let row = |y: usize| cell[y * 16];
        assert_eq!([row(0), row(1), row(2), row(3)], [0, 255, 255, 0]);

        // A tall image is pillarboxed instead.
        let cell = fit_cell(&[255u8; 2 * 4 * 4], 2, 4, 4, 4);
        assert_eq!([cell[0], cell[4], cell[8], cell[12]], [0, 255, 255, 0]);

        // Same shape, different size: every cell pixel comes from the source.
        let cell = fit_cell(&[200u8; 8 * 4 * 4], 8, 4, 4, 2);
        assert!(cell.chunks_exact(4).all(|pixel| pixel[0] == 200));
        assert_eq!(fit_cell(&[], 0, 0, 2, 2).len(), 16);
    }

    #[test]
    fn invalid_input_is_rejected() {
        let image = solid(100);
        let cell = |label| Cell {
            label,
            rgba: &image,
        };
        assert!(compose(&[], 3, CELL.0, CELL.1).is_err());
        assert!(compose(&[cell("1")], 0, CELL.0, CELL.1).is_err());
        assert!(compose(&[cell("a")], 1, CELL.0, CELL.1).is_err());
        assert!(compose(&[cell("1234567890")], 1, CELL.0, CELL.1).is_err());
        assert!(compose(&[cell("1")], 1, CELL.0 + 1, CELL.1).is_err());
    }
}

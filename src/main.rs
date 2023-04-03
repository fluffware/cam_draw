use clap::Parser;
use paths::coords::{Point, Transform};
use paths::curve_approx::CurveInfo;
use paths::curves;
use paths::stepper_context::CurveSegment;
use paths::svg_parser;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

fn curve_segment_to_info(
    seg: &CurveSegment,
    current_pos: &mut Point,
) -> Option<Box<dyn CurveInfo>> {
    Some(match seg {
        CurveSegment::LineTo(p2) | CurveSegment::CloseTo(p2) => {
            let rel = *p2 - *current_pos;
            *current_pos = *p2;
            Box::new(curves::line::Line::new(rel))
        }
        CurveSegment::CurveTo(p2, c1, c2) => {
            let rel = *p2 - *current_pos;
            *current_pos = *p2;
            Box::new(curves::bezier::Bezier::new(*c1, rel + *c2, rel))
        }
        CurveSegment::Arc(rx, ry, start, end, _rot) => {
            // Only circles are supported
            if (*rx - *ry).abs() > f64::EPSILON {
                return None;
            }
            let circle = curves::circle_segment::CircleSegment::new(*rx, *start, *end);
            let (end, _) = circle.value(circle.length());
            *current_pos += end;
            Box::new(circle)
        }
        _ => return None,
    })
}

fn svg_prologue<W: io::Write>(w: &mut W) -> std::io::Result<usize> {
    let width = 100.0;
    let height = 100.0;
    w.write(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<svg xmlns="http://www.w3.org/2000/svg"
     width="{width}mm" height="{height}mm" viewBox="{} {} {height} {height}">
"#,
            -height / 2.0,
            -width / 2.0
        )
        .as_bytes(),
    )
}

fn svg_epilogue<W: io::Write>(w: &mut W) -> std::io::Result<usize> {
    w.write(b"</svg>\n")
}
/// Open file for writing. If filename is "-" then use stdout.
fn create_output_file(filename: &PathBuf) -> Result<Box<dyn io::Write>, String> {
    if filename.as_os_str() != "-" {
        let file = match File::create(filename) {
            Ok(f) => f,
            Err(e) => {
                return Err(format!("Failed to create '{}': {}", filename.display(), e));
            }
        };
        Ok(Box::new(file))
    } else {
        Ok(Box::new(std::io::stdout()))
    }
}

#[derive(Parser, Debug)]
struct CmdArgs {
    /// SVG file defining the curve
    svg_file: Option<PathBuf>,
    /// SVG output file
    #[arg(long, short = 'o')]
    svg_output: Option<PathBuf>,
    /// LDraw output file
    #[arg(long, short = 'l')]
    ldraw_output: Option<PathBuf>,
    /// Output SVG template for curve
    #[arg(long)]
    svg_template: Option<PathBuf>,
}

struct LdrawCoord {
    x: f64,
    y: f64,
    z: f64,
}

impl LdrawCoord {
    fn xy_z(xy: &Point, z: f64) -> LdrawCoord {
        LdrawCoord {
            x: xy.x,
            y: xy.y,
            z,
        }
    }
}

impl std::fmt::Display for LdrawCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.3} {:.3} {:.3}", self.x, self.y, self.z)
    }
}

fn add_file_suffix<S: AsRef<OsStr>>(
    path: &Path,
    suffix: S,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let parent = path.parent().ok_or_else(|| "Invalid output file name")?;
    let prefix = path.file_stem().ok_or_else(|| "Invalid output file name")?;
    let ext = path.extension();
    let mut filename = OsString::from(prefix);
    filename.push(suffix);
    let mut filepath = PathBuf::from(filename);
    if let Some(ext) = ext {
        filepath.set_extension(ext);
    }
    Ok(parent.join(filepath))
}

fn write_ldraw_file(
    path: &Vec<Point>,
    filename: &PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let mut out = create_output_file(filename)?;
    let lower = 0.0;
    let upper = 20.0;
    let scale = 20.0 / 8.0;
    let radius = 6.0;
    writeln!(&mut out, "0 BFC CERTIFY CCW")?;
    if let Some(mut prev) = &path.last().map(|p| (*p * scale).clone()) {
        for p in path {
            let p = &(*p * scale);
            let c = *p * (radius / p.length());
            let prev_c = prev * (radius / prev.length());
            writeln!(
                &mut out,
                "4 16 {} {} {} {}",
                LdrawCoord::xy_z(&prev, upper),
                LdrawCoord::xy_z(p, upper),
                LdrawCoord::xy_z(p, lower),
                LdrawCoord::xy_z(&prev, lower),
            )?;
            writeln!(
                &mut out,
                "4 16 {} {} {} {}",
                LdrawCoord::xy_z(&prev, upper),
                LdrawCoord::xy_z(&prev_c, upper),
                LdrawCoord::xy_z(&c, upper),
                LdrawCoord::xy_z(&p, upper),
            )?;
            writeln!(
                &mut out,
                "4 16 {} {} {} {}",
                LdrawCoord::xy_z(&prev, lower),
                LdrawCoord::xy_z(&prev_c, lower),
                LdrawCoord::xy_z(&c, lower),
                LdrawCoord::xy_z(&p, lower),
            )?;
            writeln!(
                &mut out,
                "4 16 {} {} {} {}",
                LdrawCoord::xy_z(&prev_c, upper),
                LdrawCoord::xy_z(&prev_c, lower),
                LdrawCoord::xy_z(&c, lower),
                LdrawCoord::xy_z(&c, upper),
            )?;

            prev = p.clone();
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let args = CmdArgs::parse();
    let file: Box<dyn io::Read>;
    if let Some(svg_file) = &args.svg_file {
        match File::open(&svg_file) {
            Ok(f) => file = Box::new(f),
            Err(e) => {
                return Err(format!("Failed to open '{}': {}", svg_file.display(), e).into());
            }
        }
    } else {
        file = Box::new(std::io::stdin());
    }

    if let Some(template) = args.svg_template {
        let mut svg_out = create_output_file(&template)?;
        svg_prologue(&mut svg_out)?;
        svg_epilogue(&mut svg_out)?;
        return Ok(());
    }

    let transform = Transform::identity();
    let segs = svg_parser::parse_document(file, &transform, Box::new(|_, _| true)).unwrap();
    println!("{:?}", segs);
    //let min_cos_connect = 0.9;
    let mut current_pos = Point::from((0, 0));
    let mut start = current_pos;
    //let mut prev_dir: Option<Vector> = None;
    let mut curves = curves::concat_curve::ConcatCurve::new();

    for seg in &segs {
        let mut next_pos = current_pos;
        match *seg {
            CurveSegment::GoTo(p) => {
                // Skip gotos to the current position
                current_pos = p;
                start = current_pos;
                //prev_dir = None;
            }
            CurveSegment::CloseTo(p2) if (current_pos - p2).length() < 1e-6 => {
                // Skip short closing lines
                current_pos = p2;
            }
            _ => {
                if let Some(info) = curve_segment_to_info(seg, &mut next_pos) {
                    let (_, _start_dir) = info.value(0.0);
                    /*
                        if let Some(prev_dir) = prev_dir {
                            println!("Direction: {} -> {}", prev_dir, start_dir);
                            if start_dir.x * prev_dir.x + start_dir.y * prev_dir.y < min_cos_connect {
                                println!("Splitting curve");
                                start = current_pos;
                            }
                    }
                    */
                    let (_, _end_dir) = info.value(info.length());
                    current_pos = next_pos;
                    //prev_dir = Some(end_dir);
                    curves.add(info);
                } else {
                    panic!("Unhandled CurveSegment type")
                }
            }
        }
    }
    println!("{:?}", curves);
    let full_turn = 400;
    let length = curves.length();
    let lu = 8.0;
    let d = 8.0 * lu;
    let f = 6.0 * lu;
    let g = 3.0 * lu;
    let h = 7.0 * lu;
    let follower_radius = 0.5 * lu;
    let p0 = Point { x: h, y: 0.0 };

    let mut path1 = Vec::new();
    let mut path2 = Vec::new();
    let mut prev_pr = None;
    for rot in 0..=full_turn {
        let pos = f64::from(rot) * length / f64::from(full_turn);
        let (p1, _) = curves.value(pos);
        let p10 = p1 + Point { x: d, y: 0.0 } + start;
        //println!("p10: {p10}");
        let b = p10.length() * 0.5;
        let a = (d * d - b * b).sqrt();
        let p40 = (-p10 * 0.5 + p10.rotate_90_ccw().unit() * a).unit();
        //println!("p40: {p40}");
        let p50 = (-p10 * 0.5 + p10.rotate_90_cw().unit() * a).unit();

        let p2 = p40 * f + p40.rotate_90_ccw() * g + p0;
        //println!("p2: {p2}");
        let p3 = p50 * f + p50.rotate_90_cw() * g + p0;
        let p2r =
            Transform::rotate(f64::from(rot) * 2.0 * std::f64::consts::PI / f64::from(full_turn))
                * p2;
        let p3r =
            Transform::rotate(f64::from(rot) * 2.0 * std::f64::consts::PI / f64::from(full_turn))
                * p3;

        if let Some((prev_p2r, prev_p3r)) = prev_pr {
            // Shift curve towards center to compenasate for the follower radius
            let p2i =
                (p2r - prev_p2r).rotate_90_ccw().unit() * follower_radius + (p2r + prev_p2r) * 0.5;
            path1.push(p2i);
            let p3i =
                (p3r - prev_p3r).rotate_90_ccw().unit() * follower_radius + (p3r + prev_p3r) * 0.5;
            path2.push(p3i);
        }
        prev_pr = Some((p2r, p3r));
    }
    if let Some(svg_filename) = &args.svg_output {
        let mut svg_out = create_output_file(svg_filename)?;
        svg_prologue(&mut svg_out)?;
        write!(&mut svg_out, "<path style=\"fill:none;stroke:black\" d=\"M")?;
        for p in path1 {
            write!(&mut svg_out, " {}, {}", p.x, p.y)?;
        }
        writeln!(&mut svg_out, " z\"/>")?;

        write!(&mut svg_out, "<path style=\"fill:none;stroke:black\" d=\"M")?;
        for p in path2 {
            write!(&mut svg_out, " {}, {}", p.x, p.y)?;
        }
        writeln!(&mut svg_out, " z\"/>")?;

        svg_epilogue(&mut svg_out)?;
    } else if let Some(ldraw_filename) = &args.ldraw_output {
        write_ldraw_file(&path1, &add_file_suffix(ldraw_filename, "_1")?)?;
        write_ldraw_file(&path2, &add_file_suffix(ldraw_filename, "_2")?)?;
    }
    Ok(())
}

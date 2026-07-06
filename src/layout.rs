use anyhow::{anyhow, Context, Result};

use crate::geometry::{RectNorm, RectPx, SurfaceSize};

pub(crate) struct Layout {
    pub(crate) slots: HashMap<String, Slot>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct Slot {
    pub(crate) id: String,
    pub(crate) rect: RectNorm,
    pub(crate) role: SlotRole,
    pub(crate) capture: bool,
    pub(crate) label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SlotRole {
    Key,
    Zone,
    GestureArea,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SlotTarget {
    pub(crate) id: String,
    pub(crate) rect: RectNorm,
    pub(crate) role: SlotRole,
    pub(crate) capture: bool,
    pub(crate) label: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SlotRegistry {
    layout: Layout,
}

impl Default for SlotRegistry {
    fn default() -> Self {
        Self {
            layout: Layout {
                pub(crate) slots: HashMap::new(),
            },
        }
    }
}

#[derive(Clone, Debug, Default)]

impl SlotRegistry {
    fn from_svg_file(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read SVG layout {}", path.display()))?;
        Self::from_svg_str(&source).with_context(|| format!("parse SVG layout {}", path.display()))
    }

    fn from_svg_str(source: &str) -> Result<Self> {
        let document = roxmltree::Document::parse(source).context("parse SVG XML")?;
        let root = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "svg")
            .ok_or_else(|| anyhow!("SVG layout is missing root <svg> element"))?;
        let (view_x, view_y, view_w, view_h) = svg_canvas(root)?;
        let mut registry = Self {
            layout: Layout {
                pub(crate) slots: HashMap::new(),
            },
        };

        for node in document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "rect")
        {
            let Some(slot_id) = node.attribute("data-td-slot") else {
                continue;
            };

            let x = svg_number_attr(node, "x")?;
            let y = svg_number_attr(node, "y")?;
            let width = svg_number_attr(node, "width")?;
            let height = svg_number_attr(node, "height")?;
            let rect = validate_rect(
                RectNorm {
                    x0: (x - view_x) / view_w,
                    y0: (y - view_y) / view_h,
                    x1: (x + width - view_x) / view_w,
                    y1: (y + height - view_y) / view_h,
                },
                "SVG slot",
            )?;
            let role = parse_slot_role(node.attribute("data-td-role"))?;
            let capture = parse_optional_bool(node.attribute("data-td-capture"))?.unwrap_or(true);
            let label = node.attribute("data-td-label");
            let id = normalize_name(slot_id);

            if registry.layout.slots.contains_key(&id) {
                return Err(anyhow!("duplicate SVG slot {slot_id}"));
            }

            registry.insert_slot(&id, rect, role, capture, label);
        }

        if registry.layout.slots.is_empty() {
            return Err(anyhow!("SVG layout contains no rect with data-td-slot"));
        }

        Ok(registry)
    }

    fn get(&self, name: &str) -> Result<SlotTarget> {
        let key = normalize_name(name);
        self.layout
            .slots
            .get(&key)
            .map(|slot| SlotTarget {
                pub(crate) id: slot.id.clone(),
                pub(crate) rect: slot.rect,
                pub(crate) role: slot.role,
                pub(crate) capture: slot.capture,
                pub(crate) label: slot.label.clone(),
            })
            .ok_or_else(|| anyhow!("unknown slot {name}"))
    }

    fn slots(&self) -> impl Iterator<Item = &Slot> {
        self.layout.slots.values()
    }

    fn insert_slot(
        &mut self,
        pub(crate) name: &str,
        pub(crate) rect: RectNorm,
        pub(crate) role: SlotRole,
        pub(crate) capture: bool,
        pub(crate) label: Option<&str>,
    ) {
        let id = normalize_name(name);
        self.layout.slots.insert(
            id.clone(),
            Slot {
                id,
                rect,
                role,
                capture,
                pub(crate) label: label.map(str::to_string),
            },
        );
    }
}


pub(crate) fn svg_canvas(root: roxmltree::Node<'_, '_>) -> Result<(f64, f64, f64, f64)> {
    if let Some(view_box) = root.attribute("viewBox") {
        let values = view_box
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|value| !value.is_empty())
            .map(parse_svg_number)
            .collect::<Result<Vec<_>>>()?;
        if values.len() != 4 {
            return Err(anyhow!("SVG viewBox must contain four numbers"));
        }
        if values[2] <= 0.0 || values[3] <= 0.0 {
            return Err(anyhow!("SVG viewBox width/height must be positive"));
        }
        return Ok((values[0], values[1], values[2], values[3]));
    }

    let width = svg_number_attr(root, "width")?;
    let height = svg_number_attr(root, "height")?;
    if width <= 0.0 || height <= 0.0 {
        return Err(anyhow!("SVG width/height must be positive"));
    }
    Ok((0.0, 0.0, width, height))
}

fn svg_number_attr(node: roxmltree::Node<'_, '_>, name: &str) -> Result<f64> {
    parse_svg_number(
        node.attribute(name)
            .ok_or_else(|| anyhow!("SVG <{}> is missing {name}", node.tag_name().name()))?,
    )
}

fn parse_svg_number(value: &str) -> Result<f64> {
    let value = value.trim();
    if value.is_empty() || value.contains('%') {
        return Err(anyhow!("unsupported SVG numeric value {value:?}"));
    }
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    value
        .parse::<f64>()
        .with_context(|| format!("parse SVG numeric value {value:?}"))
}

pub(crate) fn parse_slot_role(value: Option<&str>) -> Result<SlotRole> {
    match value.map(normalize_name).as_deref() {
        None | Some("") | Some("zone") => Ok(SlotRole::Zone),
        Some("key") => Ok(SlotRole::Key),
        Some("gesture") | Some("gesture_area") => Ok(SlotRole::GestureArea),
        Some(other) => Err(anyhow!("unknown SVG slot role {other}")),
    }
}

pub(crate) fn parse_optional_bool(value: Option<&str>) -> Result<Option<bool>> {
    match value.map(normalize_name).as_deref() {
        None | Some("") => Ok(None),
        Some("1") | Some("true") | Some("yes") | Some("on") => Ok(Some(true)),
        Some("0") | Some("false") | Some("no") | Some("off") => Ok(Some(false)),
        Some(other) => Err(anyhow!("invalid boolean value {other}")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

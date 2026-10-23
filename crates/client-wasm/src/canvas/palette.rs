use web_sys::Document;

#[derive(Clone, Copy)]
pub struct CanvasPalette {
    pub background: &'static str,
    pub grid: &'static str,
    pub axis: &'static str,
    pub terrain: &'static str,
    pub terrain_stroke: &'static str,
    pub path: &'static str,
    pub preview: &'static str,
    pub team_one: &'static str,
    pub team_two: &'static str,
    pub dead: &'static str,
    pub soldier_stroke: &'static str,
    pub hit: &'static str,
}

pub fn is_dark_theme(document: &Document) -> bool {
    document
        .document_element()
        .and_then(|el| el.get_attribute("data-theme"))
        .map(|theme| theme == "dark")
        .unwrap_or(false)
}

pub fn canvas_palette(document: &Document) -> CanvasPalette {
    if is_dark_theme(document) {
        CanvasPalette {
            background: "#121514",
            grid: "rgba(78, 168, 132, .14)",
            axis: "#4ea884",
            terrain: "#1c382e",
            terrain_stroke: "#4ea884",
            path: "#ff6b57",
            preview: "#8a9287",
            team_one: "#e5b338",
            team_two: "#ff6b57",
            dead: "#444b46",
            soldier_stroke: "#121514",
            hit: "#ff6b57",
        }
    } else {
        CanvasPalette {
            background: "#f4ecd8",
            grid: "rgba(16, 37, 31, .12)",
            axis: "#10251f",
            terrain: "#244a3b",
            terrain_stroke: "#1c1f1b",
            path: "#a33a2b",
            preview: "#666756",
            team_one: "#d8a52b",
            team_two: "#a33a2b",
            dead: "#6d7168",
            soldier_stroke: "#10251f",
            hit: "#a33a2b",
        }
    }
}

//! Contracts and result types for the commands in `cmd/sheet_import.rs`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Open a flat-pattern DXF (bend lines on Nck-DG / Nck-DF) as a sheet-metal part: fold it back to
/// its outside dimensions and build a sheet rule, a base flange and edge flanges from it.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ImportFlatParams {
    /// The DXF's text. Either this or `path`.
    #[serde(default)]
    pub text: Option<String>,
    /// A DXF file to read (when `text` is not given).
    #[serde(default)]
    pub path: Option<String>,
    /// Sheet thickness, mm (more than 0, at most 100). Default: the drawing's `T:` note, else 1.
    #[serde(default)]
    pub thickness: Option<f64>,
    /// The 90° bend deduction, mm (0 to 200): the flat pattern is this much shorter than the
    /// outside dimensions at each 90° bend. The rule's K-factor comes from it with the inner
    /// radius equal to the thickness. Default: the drawing's `D:` note, else none (bends get no
    /// allowance, K-factor 0.44).
    #[serde(default)]
    pub d90: Option<f64>,
    /// The new body's name. Default: the base flange's usual name.
    #[serde(default)]
    pub name: Option<String>,
}

/// What `sheet.import_flat` returns.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportFlatResult {
    /// The sheet body built.
    pub body: String,
    /// How the pattern was drawn: one flat pattern, plates laid out apart, both, or a flat plate.
    pub mode: String,
    /// Sheet thickness used, mm.
    pub thickness: f64,
    /// The sheet rule's K-factor.
    pub k_factor: f64,
    /// The folded part's outside size along X, Y and Z, mm.
    pub size: [f64; 3],
    /// Edge flanges built (each a chain of plates off the base).
    pub flanges: usize,
    /// The outside dimension each bend makes, per plate, mm.
    pub bend_dims: Vec<f64>,
    /// The part name from the drawing's `P:` note.
    pub name: Option<String>,
    /// The material from the drawing's `M:` note.
    pub material: Option<String>,
    /// What could not be carried over, or needs checking.
    pub warnings: Vec<String>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

//! A second package linked into the same application. Hosts see it as the
//! `geometry` namespace.

use bimetal::Model;
use serde::{Deserialize, Serialize};

/// A point on a plane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Model)]
pub struct Point {
    /// Horizontal position.
    pub x: f64,
    /// Vertical position.
    pub y: f64,
}

/// Failures of geometric operations.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum GeometryError {
    /// The operation needs two different points.
    #[error("the points are the same")]
    SamePoint,
}

/// The straight-line distance between two points.
#[bimetal::export]
#[must_use]
pub fn distance(from: Point, to: Point) -> f64 {
    (to.x - from.x).hypot(to.y - from.y)
}

/// The unit vector that points from `from` towards `to`.
#[bimetal::export]
pub fn direction(from: Point, to: Point) -> Result<Point, GeometryError> {
    let length = distance(from, to);
    if length == 0.0 {
        return Err(GeometryError::SamePoint);
    }
    Ok(Point {
        x: (to.x - from.x) / length,
        y: (to.y - from.y) / length,
    })
}

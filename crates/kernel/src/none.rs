//! The kernel in builds without OpenCASCADE: nothing can be modelled.

use std::path::Path;

use cadcraft_geom::{Mesh3, Vec3};

use crate::{BoolOp, Built, KernelError, Placed, Primitive, Result, Seg};

pub fn primitive(_: &Primitive) -> Result<Built> {
    Err(KernelError::Unavailable)
}
pub fn extrude(_: &[Seg], _: Vec3) -> Result<Built> {
    Err(KernelError::Unavailable)
}
pub fn revolve(_: &[Seg], _: Vec3, _: Vec3, _: f64) -> Result<Built> {
    Err(KernelError::Unavailable)
}
pub fn boolean(_: BoolOp, _: Placed, _: Placed) -> Result<Built> {
    Err(KernelError::Unavailable)
}
pub fn fillet(_: Placed, _: f64) -> Result<Built> {
    Err(KernelError::Unavailable)
}
pub fn tessellate(_: &str) -> Result<Mesh3> {
    Err(KernelError::Unavailable)
}
pub fn export_step(_: &[Placed], _: &Path) -> Result<()> {
    Err(KernelError::Unavailable)
}
pub fn export_stl(_: &[Placed], _: &Path) -> Result<()> {
    Err(KernelError::Unavailable)
}
pub fn import_step(_: &Path) -> Result<Built> {
    Err(KernelError::Unavailable)
}

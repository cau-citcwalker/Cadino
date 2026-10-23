// The C++ side of cadcraft-kernel-occt's bridge (see src/lib.rs and src/ops.cpp).
#pragma once

// Windows headers OpenCASCADE pulls in would otherwise define min and max as macros.
#ifndef NOMINMAX
#define NOMINMAX
#endif

#include "rust/cxx.h"

#include <Standard_Failure.hxx>
#include <Standard_Type.hxx>

#include <cstdint>
#include <exception>
#include <string>

// How the bridge turns a C++ exception into a Rust `Err` (cxx's hook): OpenCASCADE's exceptions
// don't derive from std::exception, so its default would let them end the process.
namespace rust {
namespace behavior {
template <typename Try, typename Fail>
static void trycatch(Try &&func, Fail &&fail) noexcept try {
  func();
} catch (const Standard_Failure &e) {
  std::string m = std::string("OpenCASCADE ") + e.DynamicType()->Name();
  const char *what = e.GetMessageString();
  if (what != nullptr && *what != '\0') {
    m += ": ";
    m += what;
  }
  fail(m.c_str());
} catch (const std::exception &e) {
  fail(e.what());
} catch (...) {
  fail("an unknown OpenCASCADE error");
}
} // namespace behavior
} // namespace rust

namespace cadrhino {
struct SegIn;
struct WireIn;
struct ShapeIn;
struct CurveOut;
struct TopoOut;
struct PickOut;
struct SectionOut;
struct FlatOut;
struct ClashOut;
struct CheckOut;

TopoOut topology(const ShapeIn &s);
PickOut pick(const ShapeIn &s, rust::Slice<const double> origin, rust::Slice<const double> dir, double tol);
rust::String fillet_edges(const ShapeIn &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> radii);
rust::String chamfer_edges(const ShapeIn &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> d1,
                           rust::Slice<const double> d2, rust::Slice<const int32_t> faces);
rust::String extrude_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces, double distance, double taper);
rust::String offset_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces, double distance);
rust::String transform_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces, rust::Slice<const double> xf);
rust::String taper_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces, rust::Slice<const double> base,
                         rust::Slice<const double> dir, double angle);
rust::String delete_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces);
rust::Vec<rust::String> copy_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces);
rust::String imprint(const ShapeIn &s, rust::Slice<const ShapeIn> tools, rust::Slice<const WireIn> curves);
rust::Vec<rust::String> separate(const ShapeIn &s);
rust::String shell_faces(const ShapeIn &s, rust::Slice<const uint32_t> faces, double thickness);
rust::String clean(const ShapeIn &s);
CheckOut check(const ShapeIn &s);
rust::String press_pull(const ShapeIn &s, const WireIn &outer, rust::Slice<const WireIn> holes, double distance);
rust::String thicken(const ShapeIn &s, double thickness, bool both);
SectionOut section(const ShapeIn &s, rust::Slice<const double> origin, rust::Slice<const double> normal);
FlatOut flatshot(rust::Slice<const ShapeIn> shapes, rust::Slice<const double> dir, rust::Slice<const double> xdir);
rust::Vec<ClashOut> interfere(rust::Slice<const ShapeIn> first, rust::Slice<const ShapeIn> second);
rust::String sew(rust::Slice<const ShapeIn> shapes, bool solid);
rust::String mesh_shape(rust::Slice<const double> verts, rust::Slice<const uint32_t> tris, bool solid, bool merge);
rust::String to_surface(const ShapeIn &s);
rust::Vec<rust::String> split(const ShapeIn &s, rust::Slice<const ShapeIn> tools);
rust::String ruled(const WireIn &a, const WireIn &b);
rust::String tabulated(const WireIn &w, rust::Slice<const double> dir);
rust::String revolved(const WireIn &w, rust::Slice<const double> origin, rust::Slice<const double> axis, double start,
                      double angle);
rust::String coons(rust::Slice<const WireIn> edges);
rust::String planar_face(const WireIn &outer, rust::Slice<const WireIn> holes);
rust::String pipe_loft(rust::Slice<const WireIn> sections, const WireIn &spine, bool solid);
rust::String guided_loft(rust::Slice<const WireIn> sections, rust::Slice<const WireIn> guides, bool solid);
} // namespace cadrhino

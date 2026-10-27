// The OpenCASCADE operations behind cadcraft-kernel-occt's bridge (see lib.rs for the contract).
// Every function reads its shapes from B-rep text, places them in the world, and writes its result
// back as B-rep text; what OpenCASCADE throws reaches Rust as an error (see the header).

#include "cadrhino_occt.hxx"
#include "cadcraft-kernel-occt/src/lib.rs.h"

#include <algorithm>
#include <array>
#include <functional>
#include <map>
#include <cmath>
#include <cstdio>
#include <sstream>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include <BRepAdaptor_Curve.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Defeaturing.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepAlgoAPI_Section.hxx>
#include <BRepAlgoAPI_Splitter.hxx>
#include <BRepBndLib.hxx>
#include <BRepBuilderAPI_Copy.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepBuilderAPI_MakeSolid.hxx>
#include <BRepBuilderAPI_MakeVertex.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepBuilderAPI_Sewing.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepCheck.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepCheck_ListOfStatus.hxx>
#include <BRepCheck_Result.hxx>
#include <BRepClass3d_SolidClassifier.hxx>
#include <BRepExtrema_DistShapeShape.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepGProp.hxx>
#include <BRepGProp_Face.hxx>
#include <BRepLib.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRepOffsetAPI_DraftAngle.hxx>
#include <BRepOffsetAPI_MakeOffsetShape.hxx>
#include <BRepOffsetAPI_MakePipeShell.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepOffsetAPI_ThruSections.hxx>
#include <BRepOffset_MakeOffset.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepPrimAPI_MakeRevol.hxx>
#include <BRepTools.hxx>
#include <BRepTools_WireExplorer.hxx>
#include <BRep_Builder.hxx>
#include <BRep_Tool.hxx>
#include <Bnd_Box.hxx>
#include <GCPnts_AbscissaPoint.hxx>
#include <GCPnts_TangentialDeflection.hxx>
#include <GC_MakeArcOfCircle.hxx>
#include <GProp_GProps.hxx>
#include <GeomAPI_Interpolate.hxx>
#include <GeomAPI_ProjectPointOnCurve.hxx>
#include <GeomAPI_ProjectPointOnSurf.hxx>
#include <GeomConvert.hxx>
#include <GeomConvert_CompCurveToBSplineCurve.hxx>
#include <GeomFill_BSplineCurves.hxx>
#include <Geom_BSplineCurve.hxx>
#include <Geom_BSplineSurface.hxx>
#include <Geom_Line.hxx>
#include <Geom_Plane.hxx>
#include <Geom_Surface.hxx>
#include <Geom_TrimmedCurve.hxx>
#include <HLRAlgo_Projector.hxx>
#include <IGESCAFControl_Writer.hxx>
#include <IGESControl_Controller.hxx>
#include <IGESData_IGESModel.hxx>
#include <IGESSolid_ManifoldSolid.hxx>
#include <IGESSolid_Shell.hxx>
#include <Interface_Static.hxx>
#include <HLRBRep_Algo.hxx>
#include <HLRBRep_HLRToShape.hxx>
#include <IntCurvesFace_ShapeIntersector.hxx>
#include <Poly_Triangulation.hxx>
#include <ShapeAnalysis_FreeBounds.hxx>
#include <ShapeFix_Face.hxx>
#include <ShapeFix_Shape.hxx>
#include <ShapeUpgrade_UnifySameDomain.hxx>
#include <TColStd_Array1OfReal.hxx>
#include <TColgp_HArray1OfPnt.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_FormatVersion.hxx>
#include <TopTools_HSequenceOfShape.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopTools_ListIteratorOfListOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <TopoDS_Iterator.hxx>
#include <Precision.hxx>
#include <Quantity_Color.hxx>
#include <STEPCAFControl_Writer.hxx>
#include <TDataStd_Name.hxx>
#include <TDocStd_Document.hxx>
#include <XCAFDoc_ColorTool.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_LayerTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <gp.hxx>
#include <gp_Ax1.hxx>
#include <gp_Ax2.hxx>
#include <gp_Ax3.hxx>
#include <gp_Circ.hxx>
#include <gp_Elips.hxx>
#include <gp_Lin.hxx>
#include <gp_Pln.hxx>
#include <gp_Trsf.hxx>

namespace cadrhino {
namespace {

// Most points one curve's polyline may have.
const int MAX_CURVE_POINTS = 100000;
// Most messages CHECK reports.
const size_t MAX_MESSAGES = 200;

[[noreturn]] void fail(const std::string &m) { throw std::runtime_error(m); }

gp_Pnt pnt(const double *v) { return gp_Pnt(v[0], v[1], v[2]); }

gp_Pnt pnt(rust::Slice<const double> v) {
  if (v.size() < 3) fail("a point needs three numbers");
  return pnt(v.data());
}

gp_Dir dir(rust::Slice<const double> v) {
  gp_Vec w(pnt(v).XYZ());
  if (w.Magnitude() < 1e-12) fail("a direction is zero");
  return gp_Dir(w);
}

std::array<double, 3> arr(const gp_XYZ &p) { return {p.X(), p.Y(), p.Z()}; }

void push(rust::Vec<double> &out, const gp_Pnt &p) {
  out.push_back(p.X());
  out.push_back(p.Y());
  out.push_back(p.Z());
}

bool has(const TopoDS_Shape &s, TopAbs_ShapeEnum t) { return !s.IsNull() && TopExp_Explorer(s, t).More(); }

TopoDS_Shape read(const rust::String &text) {
  std::istringstream in{std::string(text)};
  TopoDS_Shape s;
  BRep_Builder b;
  BRepTools::Read(s, in, b);
  if (s.IsNull()) fail("not OpenCASCADE B-rep text");
  return s;
}

rust::String write(const TopoDS_Shape &s) {
  std::ostringstream out;
  BRepTools::Write(s, out, Standard_False, Standard_False, TopTools_FormatVersion_CURRENT);
  return rust::String(out.str());
}

gp_Trsf trsf(const double *m) {
  gp_Trsf t;
  t.SetValues(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
  return t;
}

TopoDS_Shape moved(const TopoDS_Shape &s, const gp_Trsf &t) {
  BRepBuilderAPI_Transform tr(s, t, Standard_False);
  return tr.Shape();
}

// The stored shape in the world. Moving it keeps the order its faces and edges are mapped in.
TopoDS_Shape placed(const ShapeIn &in) {
  TopoDS_Shape s = read(in.brep);
  static const double id[12] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0};
  if (std::equal(in.xf.begin(), in.xf.end(), id)) return s;
  return moved(s, trsf(in.xf.data()));
}

Bnd_Box box(const TopoDS_Shape &s) {
  Bnd_Box b;
  BRepBndLib::Add(s, b);
  return b;
}

double diagonal(const TopoDS_Shape &s) {
  Bnd_Box b = box(s);
  if (b.IsVoid()) return 0;
  double d = std::sqrt(b.SquareExtent());
  return std::isfinite(d) ? d : 0;
}

// The chord deflection polylines are drawn with (the meshes' ratio, see occt.rs).
double deflection(const TopoDS_Shape &s) {
  double d = diagonal(s);
  return d > 0 ? std::max(d * 5e-4, 1e-7) : 1e-3;
}

double volume(const TopoDS_Shape &s) {
  GProp_GProps g;
  BRepGProp::VolumeProperties(s, g);
  return g.Mass();
}

// A compound holding one shape is that shape.
TopoDS_Shape unwrap(TopoDS_Shape s) {
  for (int depth = 0; depth < 64 && s.ShapeType() == TopAbs_COMPOUND; ++depth) {
    TopoDS_Iterator it(s);
    if (!it.More()) break;
    TopoDS_Shape one = it.Value();
    it.Next();
    if (it.More()) break;
    s = one;
  }
  return s;
}

bool planar(const TopoDS_Face &f);

// Two faces on one surface: the same surface, or flat in one plane.
bool same_surface(const TopoDS_Face &a, const TopoDS_Face &b) {
  TopLoc_Location la, lb;
  Handle(Geom_Surface) sa = BRep_Tool::Surface(a, la), sb = BRep_Tool::Surface(b, lb);
  if (!sa.IsNull() && sa == sb && la.IsEqual(lb)) return true;
  if (!planar(a) || !planar(b)) return false;
  gp_Pln pa = BRepAdaptor_Surface(a, Standard_False).Plane(), pb = BRepAdaptor_Surface(b, Standard_False).Plane();
  return pa.Axis().Direction().IsParallel(pb.Axis().Direction(), 1e-9) && pa.Distance(pb.Location()) < 1e-7;
}

// The edges of `s` between two different faces on one surface.
std::vector<TopoDS_Edge> splits(const TopoDS_Shape &s) {
  TopTools_IndexedDataMapOfShapeListOfShape faces;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, faces);
  std::vector<TopoDS_Edge> out;
  for (int i = 1; i <= faces.Extent(); ++i) {
    const TopTools_ListOfShape &fs = faces(i);
    if (fs.Extent() != 2) continue;
    const TopoDS_Face &a = TopoDS::Face(fs.First()), &b = TopoDS::Face(fs.Last());
    if (!a.IsSame(b) && same_surface(a, b)) out.push_back(TopoDS::Edge(faces.FindKey(i)));
  }
  return out;
}

// Faces on one surface merged, edges on one curve joined: what an edit of `before` left split.
// An edge `before` already had between two faces on one surface (an imprint, kept on purpose:
// IMPRINT then PRESSPULL is how a cut-out is marked and made) stays wherever it still runs.
TopoDS_Shape unified(const TopoDS_Shape &s, const TopoDS_Shape &before) {
  ShapeUpgrade_UnifySameDomain u(s, Standard_True, Standard_True, Standard_False);
  std::vector<TopoDS_Edge> marks = before.IsNull() ? std::vector<TopoDS_Edge>() : splits(before);
  if (!marks.empty()) {
    double tol = std::max(1e-6, diagonal(before) * 1e-7);
    for (const TopoDS_Edge &e : splits(s)) {
      BRepAdaptor_Curve c(e);
      TopoDS_Vertex mid = BRepBuilderAPI_MakeVertex(c.Value((c.FirstParameter() + c.LastParameter()) / 2));
      for (const TopoDS_Edge &m : marks) {
        BRepExtrema_DistShapeShape d(mid, m);
        if (d.IsDone() && d.Value() < tol) {
          u.KeepShape(e);
          break;
        }
      }
    }
  }
  u.Build();
  return u.Shape();
}

// The shape checked and healed where OpenCASCADE can; null when it has no faces or stays invalid.
TopoDS_Shape healed(const TopoDS_Shape &shape) {
  TopoDS_Shape s = unwrap(shape);
  if (!has(s, TopAbs_FACE)) return TopoDS_Shape();
  if (BRepCheck_Analyzer(s).IsValid()) return s;
  Handle(ShapeFix_Shape) fix = new ShapeFix_Shape(s);
  fix->Perform();
  s = fix->Shape();
  return has(s, TopAbs_FACE) && BRepCheck_Analyzer(s).IsValid() ? s : TopoDS_Shape();
}

// The result checked (and healed where OpenCASCADE can) and written.
rust::String finish(const TopoDS_Shape &shape, const std::string &what) {
  if (!has(shape, TopAbs_FACE)) fail(what + " left nothing");
  TopoDS_Shape s = healed(shape);
  if (s.IsNull()) fail(what + " made a shape that isn't valid");
  return write(s);
}

// Faces and edges by index (from 0, in TopExp::MapShapes order).
struct Indexed {
  TopTools_IndexedMapOfShape faces, edges;
  explicit Indexed(const TopoDS_Shape &s) {
    TopExp::MapShapes(s, TopAbs_FACE, faces);
    TopExp::MapShapes(s, TopAbs_EDGE, edges);
  }
  TopoDS_Face face(uint32_t i) const {
    if (i >= static_cast<uint32_t>(faces.Extent()))
      fail("there is no face " + std::to_string(i) + " (the shape has " + std::to_string(faces.Extent()) + ")");
    return TopoDS::Face(faces.FindKey(static_cast<int>(i) + 1));
  }
  TopoDS_Edge edge(uint32_t i) const {
    if (i >= static_cast<uint32_t>(edges.Extent()))
      fail("there is no edge " + std::to_string(i) + " (the shape has " + std::to_string(edges.Extent()) + ")");
    return TopoDS::Edge(edges.FindKey(static_cast<int>(i) + 1));
  }
  std::vector<TopoDS_Face> faces_at(rust::Slice<const uint32_t> at) const {
    if (at.empty()) fail("no faces are selected");
    std::vector<TopoDS_Face> out;
    for (uint32_t i : at) out.push_back(face(i));
    return out;
  }
};

bool planar(const TopoDS_Face &f) { return BRepAdaptor_Surface(f, Standard_False).GetType() == GeomAbs_Plane; }

// A flat face's plane, its normal pointing out of the solid. The surface's own normal is X × Y
// of its frame, which a mirrored frame has opposite to its axis.
gp_Pln plane_of(const TopoDS_Face &f) {
  gp_Ax3 frame = BRepAdaptor_Surface(f, Standard_False).Plane().Position();
  gp_Dir n = frame.XDirection().Crossed(frame.YDirection());
  if (f.Orientation() == TopAbs_REVERSED) n.Reverse();
  return gp_Pln(frame.Location(), n);
}

// The face's outward normal at its point nearest `near`.
gp_Dir normal_near(const TopoDS_Face &f, const gp_Pnt &at) {
  if (planar(f)) return plane_of(f).Axis().Direction();
  double u1, u2, v1, v2;
  BRepTools::UVBounds(f, u1, u2, v1, v2);
  double u = (u1 + u2) / 2, v = (v1 + v2) / 2;
  GeomAPI_ProjectPointOnSurf proj(at, BRep_Tool::Surface(f));
  if (proj.NbPoints() > 0) proj.LowerDistanceParameters(u, v);
  BRepGProp_Face gf(f);
  gp_Pnt p;
  gp_Vec n;
  gf.Normal(u, v, p, n);
  if (n.Magnitude() < 1e-12) gf.Normal((u1 + u2) / 2, (v1 + v2) / 2, p, n);
  if (n.Magnitude() < 1e-12) return gp_Dir(0, 0, 1);
  return gp_Dir(n);
}

// --- curves in -----------------------------------------------------------------------------

TopoDS_Edge edge_of(const SegIn &s) {
  const rust::Vec<double> &v = s.v;
  auto p = [&](size_t i) {
    if (v.size() < i + 3) fail("a curve piece is missing numbers");
    return pnt(v.data() + i);
  };
  switch (s.kind) {
  case 0:
    return BRepBuilderAPI_MakeEdge(p(0), p(3)).Edge();
  case 1: {
    GC_MakeArcOfCircle arc(p(0), p(3), p(6));
    if (!arc.IsDone()) fail("an arc's three points are in a line");
    return BRepBuilderAPI_MakeEdge(arc.Value()).Edge();
  }
  case 2: {
    if (v.size() < 7) fail("a circle is missing numbers");
    gp_Vec n(p(3).XYZ());
    if (n.Magnitude() < 1e-12) fail("a circle has no normal");
    return BRepBuilderAPI_MakeEdge(gp_Circ(gp_Ax2(p(0), gp_Dir(n)), v[6])).Edge();
  }
  case 3: {
    if (v.size() < 13 || (v.size() - 7) % 3 != 0) fail("a spline needs two points");
    int n = static_cast<int>((v.size() - 7) / 3);
    Handle(TColgp_HArray1OfPnt) pts = new TColgp_HArray1OfPnt(1, n);
    for (int i = 0; i < n; ++i) pts->SetValue(i + 1, p(7 + 3 * static_cast<size_t>(i)));
    GeomAPI_Interpolate ip(pts, Standard_False, 1e-7);
    if (v[0] != 0) ip.Load(gp_Vec(p(1).XYZ()), gp_Vec(p(4).XYZ()));
    ip.Perform();
    if (!ip.IsDone()) fail("no spline runs through these points");
    return BRepBuilderAPI_MakeEdge(ip.Curve()).Edge();
  }
  default:
    fail("an unknown kind of curve piece");
  }
}

TopoDS_Wire wire_of(const WireIn &w) {
  if (w.segs.empty()) fail("a curve has no pieces");
  BRepBuilderAPI_MakeWire mw;
  for (const SegIn &s : w.segs) {
    mw.Add(edge_of(s));
    if (!mw.IsDone()) fail("the pieces of a curve must meet end to end");
  }
  return mw.Wire();
}

// A flat face: the outer curve, less the holes.
TopoDS_Face face_of(const WireIn &outer, rust::Slice<const WireIn> holes) {
  BRepBuilderAPI_MakeFace mf(wire_of(outer), Standard_True);
  if (!mf.IsDone()) fail("the outer curve doesn't bound a flat region");
  for (const WireIn &h : holes) mf.Add(wire_of(h));
  TopoDS_Face f = mf.Face();
  if (holes.empty()) return f;
  // Holes run the other way round from the outer curve.
  ShapeFix_Face fix(f);
  fix.Perform();
  return fix.Face();
}

// --- curves out ----------------------------------------------------------------------------

CurveOut curve_of(const TopoDS_Edge &e, double defl) {
  CurveOut c{};
  if (BRep_Tool::Degenerated(e)) {
    gp_Pnt p = BRep_Tool::Pnt(TopExp::FirstVertex(e));
    c.a = c.mid = c.b = arr(p.XYZ());
    return c;
  }
  BRepAdaptor_Curve ad(e);
  double f = ad.FirstParameter(), l = ad.LastParameter();
  bool rev = e.Orientation() == TopAbs_REVERSED;
  c.a = arr(ad.Value(rev ? l : f).XYZ());
  c.b = arr(ad.Value(rev ? f : l).XYZ());
  c.mid = arr(ad.Value((f + l) / 2).XYZ());
  switch (ad.GetType()) {
  case GeomAbs_Line:
    c.kind = 0;
    break;
  case GeomAbs_Circle: {
    gp_Circ ci = ad.Circle();
    c.center = arr(ci.Location().XYZ());
    gp_Dir n = ci.Axis().Direction();
    c.normal = arr((rev ? n.Reversed() : n).XYZ());
    c.radius = ci.Radius();
    c.kind = std::abs(l - f - 2 * M_PI) < 1e-9 ? 2 : 1;
    break;
  }
  case GeomAbs_Ellipse: {
    gp_Elips el = ad.Ellipse();
    c.kind = 3;
    c.center = arr(el.Location().XYZ());
    c.major = arr(el.XAxis().Direction().XYZ() * el.MajorRadius());
    c.ratio = el.MinorRadius() / el.MajorRadius();
    c.normal = arr(el.Axis().Direction().XYZ());
    c.start = f;
    c.end = l;
    break;
  }
  default: {
    c.kind = 4;
    double cf, cl;
    Handle(Geom_Curve) g = BRep_Tool::Curve(e, cf, cl);
    if (g.IsNull()) break;
    Handle(Geom_BSplineCurve) bs = GeomConvert::CurveToBSplineCurve(new Geom_TrimmedCurve(g, cf, cl));
    if (bs->IsPeriodic()) bs->SetNotPeriodic();
    if (rev) bs->Reverse();
    c.degree = static_cast<uint32_t>(bs->Degree());
    for (int i = 1; i <= bs->NbPoles(); ++i) {
      push(c.poles, bs->Pole(i));
      if (bs->IsRational()) c.weights.push_back(bs->Weight(i));
    }
    TColStd_Array1OfReal seq(1, bs->NbPoles() + bs->Degree() + 1);
    bs->KnotSequence(seq);
    for (int i = seq.Lower(); i <= seq.Upper(); ++i) c.knots.push_back(seq(i));
  }
  }
  GCPnts_TangentialDeflection td(ad, 0.1, defl, 2);
  int n = std::min(td.NbPoints(), MAX_CURVE_POINTS);
  for (int i = 1; i <= n; ++i) push(c.points, td.Value(rev ? n + 1 - i : i));
  return c;
}

WireOut wire_out(const TopoDS_Wire &w, const TopoDS_Face *on, double defl) {
  WireOut o{};
  BRepTools_WireExplorer ex;
  if (on != nullptr) ex.Init(w, *on);
  else ex.Init(w);
  for (; ex.More(); ex.Next()) o.curves.push_back(curve_of(ex.Current(), defl));
  o.closed = BRep_Tool::IsClosed(w);
  return o;
}

// Each closed shell of `sewn` made a solid facing outward.
TopoDS_Shape solids_of(const TopoDS_Shape &sewn) {
  BRep_Builder bb;
  TopoDS_Compound comp;
  bb.MakeCompound(comp);
  int closed = 0, open = 0;
  for (TopExp_Explorer ex(sewn, TopAbs_SHELL); ex.More(); ex.Next()) {
    TopoDS_Shell sh = TopoDS::Shell(ex.Current());
    if (!BRep_Tool::IsClosed(sh)) {
      ++open;
      continue;
    }
    TopoDS_Solid so = BRepBuilderAPI_MakeSolid(sh).Solid();
    BRepLib::OrientClosedSolid(so);
    bb.Add(comp, so);
    ++closed;
  }
  if (open > 0 || closed == 0)
    fail("the surfaces don't close up into a watertight shape (" + std::to_string(open) + " open pieces)");
  return comp;
}

TopoDS_Shape fused(const TopoDS_Shape &a, const TopoDS_Shape &b) {
  BRepAlgoAPI_Fuse op(a, b);
  if (!op.IsDone()) fail("joining the solids failed");
  return op.Shape();
}

TopoDS_Shape cut(const TopoDS_Shape &a, const TopoDS_Shape &b) {
  BRepAlgoAPI_Cut op(a, b);
  if (!op.IsDone()) fail("subtracting the solids failed");
  return op.Shape();
}

// The face swept `d` along `n` (into the solid when negative), its sides leaning in by `taper`
// (radians) as they leave the face.
TopoDS_Shape prism(const TopoDS_Face &f, const gp_Dir &n, double d, double taper) {
  gp_Vec along = gp_Vec(n) * d;
  BRepPrimAPI_MakePrism mp(f, along);
  TopoDS_Shape p = mp.Shape();
  if (volume(p) < 0) p.Reverse();
  if (std::abs(taper) < 1e-12) return p;
  gp_Dir pull = d > 0 ? n : n.Reversed();
  gp_Pln neutral(BRepAdaptor_Surface(f, Standard_False).Plane().Location(), pull);
  BRepOffsetAPI_DraftAngle dr(p);
  const TopoDS_Shape &first = mp.FirstShape(), &last = mp.LastShape();
  for (TopExp_Explorer ex(p, TopAbs_FACE); ex.More(); ex.Next()) {
    const TopoDS_Face &side = TopoDS::Face(ex.Current());
    if (side.IsSame(first) || side.IsSame(last)) continue;
    dr.Add(side, pull, taper, neutral);
    if (!dr.AddDone()) fail("the sides can't be tapered that much");
  }
  dr.Build();
  if (!dr.IsDone()) fail("the sides can't be tapered that much");
  return dr.Shape();
}

TopoDS_Shape offset_core(const TopoDS_Shape &s, const std::vector<std::pair<TopoDS_Face, double>> &by) {
  BRepOffset_MakeOffset mo;
  mo.Initialize(s, 0.0, 1e-7, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Intersection);
  for (const auto &fd : by) mo.SetOffsetOnFace(fd.first, fd.second);
  mo.MakeOffsetShape();
  if (!mo.IsDone()) fail("the faces can't be offset that far (error " + std::to_string(static_cast<int>(mo.Error())) + ")");
  return mo.Shape();
}

std::string draft_error(Draft_ErrorStatus s) {
  switch (s) {
  case Draft_FaceRecomputation:
    return "a face can't be tilted that far";
  case Draft_EdgeRecomputation:
    return "an edge can't follow the tilted faces";
  case Draft_VertexRecomputation:
    return "a corner can't follow the tilted faces";
  default:
    return "the faces can't be tilted";
  }
}

} // namespace

// --- topology and picking --------------------------------------------------------------------

TopoOut topology(const ShapeIn &in) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  TopTools_IndexedDataMapOfShapeListOfShape ef;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, ef);
  double defl = deflection(s);
  TopoOut out{};
  for (int i = 1; i <= ix.faces.Extent(); ++i) {
    TopoDS_Face f = TopoDS::Face(ix.faces(i));
    FaceOut fo{};
    BRepAdaptor_Surface ad(f, Standard_False);
    fo.kind = static_cast<uint8_t>(ad.GetType());
    GProp_GProps g;
    BRepGProp::SurfaceProperties(f, g);
    fo.area = g.Mass();
    gp_Pnt c = g.CentreOfMass();
    fo.centroid = arr(c.XYZ());
    fo.normal = arr(normal_near(f, c).XYZ());
    Bnd_Box b;
    BRepBndLib::AddOptimal(f, b, Standard_False, Standard_False);
    if (!b.IsVoid()) {
      fo.lo = arr(b.CornerMin().XYZ());
      fo.hi = arr(b.CornerMax().XYZ());
    }
    gp_Ax1 axis;
    switch (ad.GetType()) {
    case GeomAbs_Plane: {
      gp_Pln p = plane_of(f);
      axis = gp_Ax1(p.Location(), p.Axis().Direction());
      break;
    }
    case GeomAbs_Cylinder:
      axis = ad.Cylinder().Axis();
      fo.radius = ad.Cylinder().Radius();
      break;
    case GeomAbs_Cone:
      axis = ad.Cone().Axis();
      fo.radius = ad.Cone().RefRadius();
      break;
    case GeomAbs_Sphere:
      axis = ad.Sphere().Position().Axis();
      fo.radius = ad.Sphere().Radius();
      break;
    case GeomAbs_Torus:
      axis = ad.Torus().Axis();
      fo.radius = ad.Torus().MajorRadius();
      break;
    default:
      axis = gp_Ax1(gp::Origin(), gp_Dir(0, 0, 1));
    }
    if (ad.GetType() <= GeomAbs_Torus) {
      fo.origin = arr(axis.Location().XYZ());
      fo.axis = arr(axis.Direction().XYZ());
    }
    TopTools_IndexedMapOfShape fe;
    TopExp::MapShapes(f, TopAbs_EDGE, fe);
    for (int j = 1; j <= fe.Extent(); ++j) {
      int k = ix.edges.FindIndex(fe(j));
      if (k > 0) fo.edges.push_back(static_cast<uint32_t>(k - 1));
    }
    out.faces.push_back(std::move(fo));
  }
  for (int i = 1; i <= ix.edges.Extent(); ++i) {
    TopoDS_Edge e = TopoDS::Edge(ix.edges(i));
    EdgeOut eo{};
    eo.degenerate = BRep_Tool::Degenerated(e);
    eo.curve = curve_of(e, defl);
    if (!eo.degenerate) eo.length = GCPnts_AbscissaPoint::Length(BRepAdaptor_Curve(e));
    int k = ef.FindIndex(e);
    if (k > 0) {
      for (TopTools_ListIteratorOfListOfShape it(ef(k)); it.More(); it.Next()) {
        int fi = ix.faces.FindIndex(it.Value());
        if (fi <= 0) continue;
        uint32_t idx = static_cast<uint32_t>(fi - 1);
        if (std::find(eo.faces.begin(), eo.faces.end(), idx) != eo.faces.end()) {
          eo.seam = true;
          continue;
        }
        eo.faces.push_back(idx);
      }
      if (!eo.seam && eo.faces.size() == 1)
        eo.seam = BRep_Tool::IsClosed(e, TopoDS::Face(ix.faces(static_cast<int>(eo.faces[0]) + 1)));
    }
    out.edges.push_back(std::move(eo));
  }
  return out;
}

PickOut pick(const ShapeIn &in, rust::Slice<const double> origin, rust::Slice<const double> along,
             double tol) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  gp_Pnt o = pnt(origin);
  gp_Vec dv(pnt(along).XYZ());
  PickOut out{};
  if (dv.Magnitude() > 1e-12) {
    gp_Dir d(dv);
    gp_Lin line(o, d);
    IntCurvesFace_ShapeIntersector hit;
    hit.Load(s, 1e-7);
    hit.Perform(line, -Precision::Infinite(), Precision::Infinite());
    if (hit.IsDone()) {
      for (int i = 1; i <= hit.NbPnt(); ++i) {
        int fi = ix.faces.FindIndex(hit.Face(i));
        if (fi <= 0) continue;
        HitOut h{};
        h.index = static_cast<uint32_t>(fi - 1);
        h.point = arr(hit.Pnt(i).XYZ());
        h.along = hit.WParameter(i);
        out.faces.push_back(h);
      }
    }
    // The line, as an edge long enough to pass the whole shape.
    Bnd_Box b = box(s);
    if (b.IsVoid()) return out;
    double lo = 0, hi = 0;
    bool first = true;
    gp_Pnt mn = b.CornerMin(), mx = b.CornerMax();
    for (int k = 0; k < 8; ++k) {
      gp_Pnt c((k & 1) ? mx.X() : mn.X(), (k & 2) ? mx.Y() : mn.Y(), (k & 4) ? mx.Z() : mn.Z());
      double t = gp_Vec(o, c).Dot(gp_Vec(d));
      lo = first ? t : std::min(lo, t);
      hi = first ? t : std::max(hi, t);
      first = false;
    }
    double pad = tol + 1 + (hi - lo);
    TopoDS_Edge seg = BRepBuilderAPI_MakeEdge(o.Translated(gp_Vec(d) * (lo - pad)), o.Translated(gp_Vec(d) * (hi + pad))).Edge();
    for (int i = 1; i <= ix.edges.Extent(); ++i) {
      const TopoDS_Edge &e = TopoDS::Edge(ix.edges(i));
      if (BRep_Tool::Degenerated(e)) continue;
      Bnd_Box eb = box(e);
      eb.Enlarge(tol);
      if (eb.IsOut(line)) continue;
      BRepExtrema_DistShapeShape dist(e, seg);
      if (!dist.IsDone() || dist.NbSolution() == 0 || dist.Value() > tol) continue;
      HitOut h{};
      h.index = static_cast<uint32_t>(i - 1);
      gp_Pnt p = dist.PointOnShape1(1);
      h.point = arr(p.XYZ());
      h.along = gp_Vec(o, p).Dot(gp_Vec(d));
      h.distance = dist.Value();
      out.edges.push_back(h);
    }
    auto front = [](const HitOut &a, const HitOut &b) { return a.along < b.along; };
    std::sort(out.faces.begin(), out.faces.end(), front);
    std::sort(out.edges.begin(), out.edges.end(), front);
    return out;
  }
  TopoDS_Vertex v = BRepBuilderAPI_MakeVertex(o).Vertex();
  auto close_to = [&](const TopoDS_Shape &sub, uint32_t index, rust::Vec<HitOut> &to) {
    Bnd_Box sb = box(sub);
    sb.Enlarge(tol);
    if (sb.IsOut(o)) return;
    BRepExtrema_DistShapeShape dist(v, sub);
    if (!dist.IsDone() || dist.NbSolution() == 0 || dist.Value() > tol) return;
    HitOut h{};
    h.index = index;
    h.point = arr(dist.PointOnShape2(1).XYZ());
    h.distance = dist.Value();
    to.push_back(h);
  };
  for (int i = 1; i <= ix.faces.Extent(); ++i) close_to(ix.faces(i), static_cast<uint32_t>(i - 1), out.faces);
  for (int i = 1; i <= ix.edges.Extent(); ++i)
    if (!BRep_Tool::Degenerated(TopoDS::Edge(ix.edges(i)))) close_to(ix.edges(i), static_cast<uint32_t>(i - 1), out.edges);
  auto closest = [](const HitOut &a, const HitOut &b) { return a.distance < b.distance; };
  std::sort(out.faces.begin(), out.faces.end(), closest);
  std::sort(out.edges.begin(), out.edges.end(), closest);
  return out;
}

// --- edges ---------------------------------------------------------------------------------

rust::String fillet_edges(const ShapeIn &in, rust::Slice<const uint32_t> edges, rust::Slice<const double> radii) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  if (edges.empty() || edges.size() != radii.size()) fail("each edge needs a radius");
  BRepFilletAPI_MakeFillet mk(s);
  for (size_t i = 0; i < edges.size(); ++i) mk.Add(radii[i], ix.edge(edges[i]));
  mk.Build();
  if (!mk.IsDone()) fail("OpenCASCADE could not round these edges (try a smaller radius)");
  return finish(mk.Shape(), "rounding the edges");
}

rust::String chamfer_edges(const ShapeIn &in, rust::Slice<const uint32_t> edges, rust::Slice<const double> d1,
                           rust::Slice<const double> d2, rust::Slice<const int32_t> faces) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  size_t n = edges.size();
  if (n == 0 || d1.size() != n || d2.size() != n || faces.size() != n) fail("each edge needs two distances and a face");
  TopTools_IndexedDataMapOfShapeListOfShape ef;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, ef);
  BRepFilletAPI_MakeChamfer mk(s);
  for (size_t i = 0; i < n; ++i) {
    TopoDS_Edge e = ix.edge(edges[i]);
    const TopTools_ListOfShape &on = ef.FindFromKey(e);
    if (on.IsEmpty()) fail("edge " + std::to_string(edges[i]) + " is on no face");
    TopoDS_Face f = TopoDS::Face(on.First());
    if (faces[i] >= 0) {
      f = ix.face(static_cast<uint32_t>(faces[i]));
      bool next = false;
      for (TopTools_ListIteratorOfListOfShape it(on); it.More(); it.Next()) next = next || it.Value().IsSame(f);
      if (!next) fail("face " + std::to_string(faces[i]) + " is not next to edge " + std::to_string(edges[i]));
    }
    mk.Add(d1[i], d2[i], e, f);
  }
  mk.Build();
  if (!mk.IsDone()) fail("OpenCASCADE could not bevel these edges (try smaller distances)");
  return finish(mk.Shape(), "bevelling the edges");
}

// --- faces ---------------------------------------------------------------------------------

rust::String extrude_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces, double distance, double taper) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  std::vector<TopoDS_Face> picked = ix.faces_at(faces);
  for (const TopoDS_Face &f : picked)
    if (!planar(f)) fail("only flat faces can be extruded");
  if (!has(s, TopAbs_SOLID)) {
    // A surface: pulled into a solid either way (as EXTRUDE makes one of a planar surface), so
    // all of it, or a face would be lost.
    if (picked.size() < static_cast<size_t>(ix.faces.Extent()))
      fail("a surface is pulled whole: pick all its faces, or make it a solid first");
    TopoDS_Shape r;
    for (const TopoDS_Face &f : picked) {
      TopoDS_Shape tool = prism(f, plane_of(f).Axis().Direction(), distance, taper);
      r = r.IsNull() ? tool : fused(r, tool);
    }
    return finish(unified(r, s), "extruding the faces");
  }
  TopoDS_Shape r = s;
  for (const TopoDS_Face &f : picked) {
    TopoDS_Shape tool = prism(f, plane_of(f).Axis().Direction(), distance, taper);
    r = distance > 0 ? fused(r, tool) : cut(r, tool);
  }
  return finish(unified(r, s), "extruding the faces");
}

rust::String offset_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces, double distance) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  std::vector<std::pair<TopoDS_Face, double>> by;
  for (const TopoDS_Face &f : ix.faces_at(faces)) by.emplace_back(f, distance);
  return finish(unified(offset_core(s, by), s), "offsetting the faces");
}

rust::String transform_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces, rust::Slice<const double> xf) {
  if (xf.size() < 12) fail("a motion needs twelve numbers");
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  gp_Trsf t = trsf(xf.data());
  std::vector<TopoDS_Face> flat, curved;
  for (const TopoDS_Face &f : ix.faces_at(faces)) (planar(f) ? flat : curved).push_back(f);
  // ponytail: flat faces go to their new planes (an offset or a draft), curved ones move the
  // feature they bound (removed, moved, put back), so the two kinds go separately. A general
  // replace-the-surface-and-reintersect modification (as Draft_Modification does) if mixed sets
  // or curved faces that bound no removable feature need moving.
  if (!flat.empty() && !curved.empty()) fail("move or turn flat faces and curved faces separately");
  if (!flat.empty()) {
    // A flat face goes to its moved plane, its neighbours following: moved along its normal
    // (sliding within its plane changes nothing), or tilted about where the two planes meet.
    std::vector<std::pair<TopoDS_Face, double>> shift;
    BRepOffsetAPI_DraftAngle tilt(s);
    int tilted = 0;
    for (const TopoDS_Face &f : flat) {
      gp_Pln p = plane_of(f);
      gp_Dir n = p.Axis().Direction(), n2 = n.Transformed(t);
      gp_Pnt c = p.Location(), c2 = c.Transformed(t);
      double turn = n.Angle(n2);
      if (turn < 1e-9) {
        double d = gp_Vec(c, c2).Dot(gp_Vec(n));
        if (std::abs(d) > 1e-9) shift.emplace_back(f, d);
        continue;
      }
      if (turn > M_PI - 1e-6) fail("a face can't be turned right over");
      // The hinge: the line both planes hold.
      gp_Dir hinge = n.Crossed(n2);
      gp_Dir pull = hinge.Crossed(n); // in the face, square to the hinge
      // Where the hinge crosses the line in the old plane along `pull` through c.
      double k = gp_Vec(c2.XYZ() - c.XYZ()).Dot(gp_Vec(n2)) / gp_Vec(pull).Dot(gp_Vec(n2));
      gp_Pnt on = c.Translated(gp_Vec(pull) * k);
      // Tilted from n towards n2: about the hinge, the face leans towards -pull's side.
      tilt.Add(f, pull, turn, gp_Pln(on, pull));
      if (!tilt.AddDone()) fail(draft_error(tilt.Status()));
      ++tilted;
    }
    if (tilted > 0 && !shift.empty()) fail("a face can't both move and turn");
    if (tilted > 0) {
      tilt.Build();
      if (!tilt.IsDone()) fail(draft_error(tilt.Status()));
      return finish(unified(tilt.Shape(), s), "turning the faces");
    }
    if (shift.empty()) return finish(s, "moving the faces");
    return finish(unified(offset_core(s, shift), s), "moving the faces");
  }
  // Curved faces (a hole, a boss): the feature they bound is taken out, moved and put back.
  BRepAlgoAPI_Defeaturing df;
  df.SetShape(s);
  for (const TopoDS_Face &f : curved) df.AddFaceToRemove(f);
  df.SetRunParallel(Standard_False);
  df.SetToFillHistory(Standard_False);
  df.Build();
  if (!df.IsDone() || df.HasErrors() || df.HasWarnings()) fail("these faces don't bound a feature that can be moved");
  TopoDS_Shape filled = df.Shape();
  double v = std::abs(volume(s)), tiny = 1e-9 * std::max(v, 1e-9);
  TopoDS_Shape hole = cut(filled, s), boss = cut(s, filled), r = filled;
  if (std::abs(volume(hole)) > tiny) r = cut(r, moved(hole, t));
  if (std::abs(volume(boss)) > tiny) r = fused(r, moved(boss, t));
  return finish(unified(r, s), "moving the faces");
}

rust::String taper_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces, rust::Slice<const double> base,
                         rust::Slice<const double> along, double angle) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  gp_Dir d = dir(along);
  gp_Pln neutral(pnt(base), d);
  BRepOffsetAPI_DraftAngle dr(s);
  for (const TopoDS_Face &f : ix.faces_at(faces)) {
    dr.Add(f, d, angle, neutral);
    if (!dr.AddDone()) fail(draft_error(dr.Status()));
  }
  dr.Build();
  if (!dr.IsDone()) fail(draft_error(dr.Status()));
  return finish(dr.Shape(), "tapering the faces");
}

rust::String delete_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  BRepAlgoAPI_Defeaturing df;
  df.SetShape(s);
  for (const TopoDS_Face &f : ix.faces_at(faces)) df.AddFaceToRemove(f);
  df.SetRunParallel(Standard_False);
  df.SetToFillHistory(Standard_False);
  df.Build();
  // A face it can't remove is a warning to OpenCASCADE (it leaves the shape as it was).
  if (!df.IsDone() || df.HasErrors() || df.HasWarnings())
    fail("these faces can't be removed: the faces round them can't be extended to close the gap");
  return finish(df.Shape(), "removing the faces");
}

rust::Vec<rust::String> copy_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  rust::Vec<rust::String> out;
  for (const TopoDS_Face &f : ix.faces_at(faces)) out.push_back(write(BRepBuilderAPI_Copy(f).Shape()));
  return out;
}

// --- bodies --------------------------------------------------------------------------------

rust::String imprint(const ShapeIn &in, rust::Slice<const ShapeIn> tools, rust::Slice<const WireIn> curves) {
  TopoDS_Shape s = placed(in);
  TopTools_ListOfShape with;
  for (const WireIn &w : curves) with.Append(wire_of(w));
  for (const ShapeIn &t : tools) {
    BRepAlgoAPI_Section sec(s, placed(t), Standard_False);
    sec.Approximation(Standard_True);
    sec.Build();
    if (sec.IsDone() && has(sec.Shape(), TopAbs_EDGE)) with.Append(sec.Shape());
  }
  if (with.IsEmpty()) fail("nothing to imprint");
  TopTools_ListOfShape args;
  args.Append(s);
  BRepAlgoAPI_Splitter sp;
  sp.SetArguments(args);
  sp.SetTools(with);
  sp.Build();
  if (!sp.IsDone() || sp.HasErrors()) fail("the curves can't be imprinted");
  return finish(sp.Shape(), "imprinting");
}

rust::Vec<rust::String> separate(const ShapeIn &in) {
  TopoDS_Shape s = placed(in);
  rust::Vec<rust::String> out;
  for (TopExp_Explorer ex(s, TopAbs_SOLID); ex.More(); ex.Next()) out.push_back(write(ex.Current()));
  if (out.empty()) fail("only 3D solids can be separated");
  return out;
}

rust::String shell_faces(const ShapeIn &in, rust::Slice<const uint32_t> faces, double thickness) {
  TopoDS_Shape s = placed(in);
  Indexed ix(s);
  if (faces.empty()) {
    BRepOffsetAPI_MakeOffsetShape os;
    os.PerformByJoin(s, -thickness, 1e-7, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Intersection);
    if (!os.IsDone()) fail("the walls can't be that thick");
    // Inward the offset is the hollow; outward (negative) the solid is, inside a grown skin.
    return finish(thickness > 0 ? cut(s, os.Shape()) : cut(os.Shape(), s), "hollowing the solid");
  }
  TopTools_ListOfShape open;
  for (const TopoDS_Face &f : ix.faces_at(faces)) open.Append(f);
  BRepOffsetAPI_MakeThickSolid mk;
  mk.MakeThickSolidByJoin(s, open, -thickness, 1e-7, BRepOffset_Skin, Standard_False, Standard_False,
                          GeomAbs_Intersection);
  mk.Build();
  if (!mk.IsDone()) fail("the walls can't be that thick");
  return finish(mk.Shape(), "hollowing the solid");
}

// ponytail: merges every face on one surface, so imprinted edges on a flat face go too (AutoCAD's
// clean keeps them); ShapeUpgrade_UnifySameDomain::KeepShapes with the imprints if that matters.
rust::String clean(const ShapeIn &in) { return finish(unified(placed(in), TopoDS_Shape()), "cleaning"); }

CheckOut check(const ShapeIn &in) {
  TopoDS_Shape s = placed(in);
  CheckOut out{};
  BRepCheck_Analyzer ana(s);
  out.valid = ana.IsValid();
  const std::pair<TopAbs_ShapeEnum, const char *> kinds[] = {{TopAbs_VERTEX, "vertex"}, {TopAbs_EDGE, "edge"},
                                                             {TopAbs_WIRE, "wire"},     {TopAbs_FACE, "face"},
                                                             {TopAbs_SHELL, "shell"},   {TopAbs_SOLID, "solid"}};
  for (const auto &k : kinds) {
    TopTools_IndexedMapOfShape m;
    TopExp::MapShapes(s, k.first, m);
    for (int i = 1; i <= m.Extent() && out.messages.size() < MAX_MESSAGES; ++i) {
      const Handle(BRepCheck_Result) &r = ana.Result(m(i));
      if (r.IsNull()) continue;
      for (BRepCheck_ListIteratorOfListOfStatus it(r->Status()); it.More(); it.Next()) {
        if (it.Value() == BRepCheck_NoError) continue;
        std::ostringstream ss;
        BRepCheck::Print(it.Value(), ss);
        std::string what = ss.str();
        while (!what.empty() && (what.back() == '\n' || what.back() == '\r' || what.back() == ' ')) what.pop_back();
        out.messages.push_back(rust::String(std::string(k.second) + " " + std::to_string(i - 1) + ": " + what));
      }
    }
  }
  if (!has(s, TopAbs_SOLID)) {
    out.valid = false;
    out.messages.push_back(rust::String("it is a surface, not a 3D solid"));
  }
  for (TopExp_Explorer ex(s, TopAbs_SOLID); ex.More() && out.messages.size() < MAX_MESSAGES; ex.Next()) {
    if (volume(ex.Current()) < 0) {
      out.valid = false;
      out.messages.push_back(rust::String("solid: inside out (its faces point inward)"));
    }
  }
  return out;
}

rust::String press_pull(const ShapeIn &in, const WireIn &outer, rust::Slice<const WireIn> holes, double distance) {
  TopoDS_Shape s = placed(in);
  TopoDS_Face f = face_of(outer, holes);
  gp_Dir n = plane_of(f).Axis().Direction();
  // A point inside the region: the middle of one of its triangles.
  BRepMesh_IncrementalMesh mesher(f, std::max(diagonal(f) * 0.01, 1e-6));
  TopLoc_Location loc;
  Handle(Poly_Triangulation) tri = BRep_Tool::Triangulation(f, loc);
  if (tri.IsNull() || tri->NbTriangles() == 0) fail("the region has no area");
  int a, b, c;
  tri->Triangle(1).Get(a, b, c);
  gp_Pnt mid((tri->Node(a).XYZ() + tri->Node(b).XYZ() + tri->Node(c).XYZ()) / 3);
  mid.Transform(loc.Transformation());
  // The solid lies on one side of the region: it is pulled out from the other.
  double eps = std::max(1e-6, diagonal(s) * 1e-6);
  BRepClass3d_SolidClassifier cls(s);
  cls.Perform(mid.Translated(gp_Vec(n) * eps), 1e-7);
  bool plus = cls.State() == TopAbs_IN;
  cls.Perform(mid.Translated(gp_Vec(n) * -eps), 1e-7);
  bool minus = cls.State() == TopAbs_IN;
  if (plus == minus) fail(plus ? "the region lies inside the solid" : "the region is not on a face of the solid");
  gp_Dir out = plus ? n.Reversed() : n;
  TopoDS_Shape tool = prism(f, out, distance, 0);
  return finish(unified(distance > 0 ? fused(s, tool) : cut(s, tool), s), "pressing or pulling");
}

rust::String thicken(const ShapeIn &in, double thickness, bool both) {
  TopoDS_Shape s = placed(in);
  if (has(s, TopAbs_SOLID)) fail("only surfaces can be thickened");
  TopoDS_Shape base = s;
  if (both) {
    BRepOffsetAPI_MakeOffsetShape os;
    os.PerformBySimple(s, -thickness / 2);
    if (!os.IsDone()) fail("the surface can't be offset that far");
    base = os.Shape();
  }
  BRepOffsetAPI_MakeThickSolid mk;
  mk.MakeThickSolidBySimple(base, thickness);
  mk.Build();
  if (!mk.IsDone()) fail("the surface can't be thickened that much");
  // Thickened along the face's normal it can come out inside out: turned the right way.
  TopoDS_Shape r = mk.Shape();
  if (volume(r) < 0) r.Reverse();
  return finish(r, "thickening");
}

// --- sections and views --------------------------------------------------------------------

SectionOut section(const ShapeIn &in, rust::Slice<const double> origin, rust::Slice<const double> normal) {
  TopoDS_Shape s = placed(in);
  gp_Pnt o = pnt(origin);
  gp_Dir n = dir(normal);
  Bnd_Box b = box(s);
  if (b.IsVoid()) fail("the shape is empty");
  gp_Pnt centre((b.CornerMin().XYZ() + b.CornerMax().XYZ()) / 2);
  double r = 2 * (diagonal(s) + o.Distance(centre)) + 1;
  TopoDS_Face plane = BRepBuilderAPI_MakeFace(gp_Pln(o, n), -r, r, -r, r).Face();
  double defl = deflection(s);
  SectionOut out{};
  if (has(s, TopAbs_SOLID)) {
    BRepAlgoAPI_Common com(s, plane);
    if (!com.IsDone()) fail("cutting the solid failed");
    for (TopExp_Explorer ex(com.Shape(), TopAbs_FACE); ex.More(); ex.Next()) {
      TopoDS_Face f = TopoDS::Face(ex.Current());
      RegionOut reg{};
      reg.brep = write(f);
      TopoDS_Wire outer = BRepTools::OuterWire(f);
      if (!outer.IsNull()) reg.wires.push_back(wire_out(outer, &f, defl));
      for (TopExp_Explorer w(f, TopAbs_WIRE); w.More(); w.Next())
        if (!w.Current().IsSame(outer)) reg.wires.push_back(wire_out(TopoDS::Wire(w.Current()), &f, defl));
      out.regions.push_back(std::move(reg));
    }
  }
  BRepAlgoAPI_Section sec(s, plane, Standard_False);
  sec.Approximation(Standard_True);
  sec.Build();
  if (!sec.IsDone()) fail("cutting the shape failed");
  Handle(TopTools_HSequenceOfShape) edges = new TopTools_HSequenceOfShape, wires;
  for (TopExp_Explorer ex(sec.Shape(), TopAbs_EDGE); ex.More(); ex.Next()) edges->Append(ex.Current());
  ShapeAnalysis_FreeBounds::ConnectEdgesToWires(edges, 1e-6, Standard_False, wires);
  for (int i = 1; !wires.IsNull() && i <= wires->Length(); ++i)
    out.wires.push_back(wire_out(TopoDS::Wire(wires->Value(i)), nullptr, defl));
  return out;
}

FlatOut flatshot(rust::Slice<const ShapeIn> shapes, rust::Slice<const double> toward, rust::Slice<const double> xdir) {
  if (shapes.empty()) fail("nothing to draw");
  Handle(HLRBRep_Algo) algo = new HLRBRep_Algo();
  double size = 0;
  for (const ShapeIn &in : shapes) {
    TopoDS_Shape s = placed(in);
    size = std::max(size, diagonal(s));
    algo->Add(s);
  }
  gp_Dir d = dir(toward), x = dir(xdir);
  if (d.IsParallel(x, 1e-9)) fail("the view's right direction runs along the view");
  algo->Projector(HLRAlgo_Projector(gp_Ax2(gp::Origin(), d, x)));
  algo->Update();
  algo->Hide();
  HLRBRep_HLRToShape h(algo);
  double defl = std::max(size * 5e-4, 1e-7);
  FlatOut out{};
  auto add = [&](const TopoDS_Shape &c, rust::Vec<CurveOut> &to) {
    if (c.IsNull()) return;
    BRepLib::BuildCurves3d(c);
    for (TopExp_Explorer ex(c, TopAbs_EDGE); ex.More(); ex.Next()) to.push_back(curve_of(TopoDS::Edge(ex.Current()), defl));
  };
  // ponytail: sharp edges and silhouettes; smooth edges (a fillet running into a face) only as
  // silhouettes, as drawings show them here. Rg1LineVCompound adds them if FLATSHOT wants them.
  add(h.VCompound(), out.visible);
  add(h.OutLineVCompound(), out.visible);
  add(h.HCompound(), out.hidden);
  add(h.OutLineHCompound(), out.hidden);
  return out;
}

rust::Vec<ClashOut> interfere(rust::Slice<const ShapeIn> first, rust::Slice<const ShapeIn> second) {
  struct Part {
    TopoDS_Shape s;
    Bnd_Box b;
    double v;
  };
  auto load = [](rust::Slice<const ShapeIn> in) {
    std::vector<Part> out;
    for (const ShapeIn &x : in) {
      TopoDS_Shape s = placed(x);
      out.push_back(Part{s, box(s), std::abs(volume(s))});
    }
    return out;
  };
  std::vector<Part> a = load(first), b = load(second);
  bool within = b.empty();
  const std::vector<Part> &other = within ? a : b;
  rust::Vec<ClashOut> out;
  for (size_t i = 0; i < a.size(); ++i) {
    for (size_t j = within ? i + 1 : 0; j < other.size(); ++j) {
      if (a[i].b.IsOut(other[j].b)) continue;
      BRepAlgoAPI_Common com(a[i].s, other[j].s);
      if (!com.IsDone() || !has(com.Shape(), TopAbs_SOLID)) continue;
      double v = std::abs(volume(com.Shape()));
      if (v <= 1e-9 * std::min(a[i].v, other[j].v) || v <= 1e-12) continue;
      ClashOut c{};
      c.a = static_cast<uint32_t>(i);
      c.b = static_cast<uint32_t>(j);
      c.brep = write(unwrap(com.Shape()));
      c.volume = v;
      out.push_back(std::move(c));
    }
  }
  return out;
}

// --- conversion ----------------------------------------------------------------------------

rust::String sew(rust::Slice<const ShapeIn> shapes, bool solid) {
  if (shapes.empty()) fail("nothing to join");
  std::vector<TopoDS_Shape> all;
  double size = 0;
  for (const ShapeIn &in : shapes) {
    all.push_back(placed(in));
    size = std::max(size, diagonal(all.back()));
  }
  BRepBuilderAPI_Sewing sw(std::max(1e-6, size * 1e-7));
  for (const TopoDS_Shape &s : all) sw.Add(s);
  sw.Perform();
  TopoDS_Shape r = sw.SewedShape();
  return finish(solid ? solids_of(r) : r, "joining the surfaces");
}

rust::String mesh_shape(rust::Slice<const double> verts, rust::Slice<const uint32_t> tris, bool solid, bool merge) {
  size_t nv = verts.size() / 3;
  std::vector<gp_Pnt> p;
  Bnd_Box b;
  for (size_t i = 0; i < nv; ++i) {
    p.push_back(pnt(verts.data() + 3 * i));
    b.Add(p.back());
  }
  double size = b.IsVoid() ? 0 : std::sqrt(b.SquareExtent());
  double tol = std::max(1e-7, size * 1e-9);
  // Corners at one point are one corner (a mesh made face by face repeats them).
  std::map<std::array<long long, 3>, uint32_t> at;
  std::vector<uint32_t> same(nv);
  for (size_t i = 0; i < nv; ++i) {
    std::array<long long, 3> k = {std::llround(p[i].X() / tol), std::llround(p[i].Y() / tol), std::llround(p[i].Z() / tol)};
    same[i] = at.emplace(k, static_cast<uint32_t>(i)).first->second;
  }
  // The triangles with area, by their corners.
  std::vector<std::array<uint32_t, 3>> t;
  for (size_t i = 0; i + 2 < tris.size(); i += 3) {
    if (tris[i] >= nv || tris[i + 1] >= nv || tris[i + 2] >= nv) fail("a triangle has a corner that isn't in the mesh");
    std::array<uint32_t, 3> c = {same[tris[i]], same[tris[i + 1]], same[tris[i + 2]]};
    if (c[0] == c[1] || c[1] == c[2] || c[2] == c[0]) continue;
    if (gp_Vec(p[c[0]], p[c[1]]).Crossed(gp_Vec(p[c[0]], p[c[2]])).Magnitude() <= 1e-12 * size * size) continue;
    t.push_back(c);
  }
  if (t.empty()) fail("the mesh has no faces with area");
  // The triangles round each side, then each piece of the mesh turned one way round: across a
  // side two triangles run it in opposite directions (where only two share it).
  auto key = [](uint32_t a, uint32_t b) { return std::make_pair(std::min(a, b), std::max(a, b)); };
  std::map<std::pair<uint32_t, uint32_t>, std::vector<size_t>> round;
  for (size_t i = 0; i < t.size(); ++i)
    for (int k = 0; k < 3; ++k) round[key(t[i][k], t[i][(k + 1) % 3])].push_back(i);
  auto runs = [&](size_t i, uint32_t a, uint32_t b) {
    for (int k = 0; k < 3; ++k)
      if (t[i][k] == a && t[i][(k + 1) % 3] == b) return true;
    return false;
  };
  std::vector<int> piece(t.size(), -1);
  int pieces = 0;
  for (size_t first = 0; first < t.size(); ++first) {
    if (piece[first] >= 0) continue;
    std::vector<size_t> todo{first};
    piece[first] = pieces;
    while (!todo.empty()) {
      size_t i = todo.back();
      todo.pop_back();
      for (int k = 0; k < 3; ++k) {
        uint32_t a = t[i][k], c = t[i][(k + 1) % 3];
        const std::vector<size_t> &by = round[key(a, c)];
        if (by.size() != 2) continue;
        size_t j = by[0] == i ? by[1] : by[0];
        if (piece[j] >= 0) continue;
        if (runs(j, a, c)) std::swap(t[j][1], t[j][2]);
        piece[j] = pieces;
        todo.push_back(j);
      }
    }
    ++pieces;
  }
  // Each closed piece (every side shared by two of its triangles) turned to face outward: its
  // volume positive.
  std::vector<double> vol(pieces, 0.0);
  std::vector<char> shut(pieces, 1);
  gp_XYZ o = b.CornerMin().XYZ();
  for (size_t i = 0; i < t.size(); ++i)
    vol[piece[i]] += (p[t[i][0]].XYZ() - o).Dot((p[t[i][1]].XYZ() - o).Crossed(p[t[i][2]].XYZ() - o));
  for (const auto &kv : round)
    if (kv.second.size() != 2)
      for (size_t i : kv.second) shut[piece[i]] = 0;
  for (size_t i = 0; i < t.size(); ++i)
    if (shut[piece[i]] && vol[piece[i]] < 0) std::swap(t[i][1], t[i][2]);
  // One vertex per corner and one edge per side, shared by the triangles round them, so the
  // faces join without sewing (which took a minute on 50,000 triangles).
  // ponytail: one flat face per triangle (merged where neighbours lie in one plane); a fitted
  // NURBS surface (Rhino's MeshToNURB with smoothing) if converted meshes should come out smooth.
  BRep_Builder bb;
  std::vector<TopoDS_Vertex> vx(nv);
  auto vertex = [&](uint32_t i) {
    if (vx[i].IsNull()) bb.MakeVertex(vx[i], p[i], tol);
    return vx[i];
  };
  std::map<std::pair<uint32_t, uint32_t>, TopoDS_Edge> sides;
  auto side = [&](uint32_t a, uint32_t c) {
    auto k = key(a, c);
    auto it = sides.find(k);
    if (it == sides.end()) it = sides.emplace(k, BRepBuilderAPI_MakeEdge(vertex(k.first), vertex(k.second)).Edge()).first;
    return a < c ? it->second : TopoDS::Edge(it->second.Reversed());
  };
  std::vector<TopoDS_Shell> shells(pieces);
  for (TopoDS_Shell &sh : shells) bb.MakeShell(sh);
  for (size_t i = 0; i < t.size(); ++i) {
    const std::array<uint32_t, 3> &c = t[i];
    TopoDS_Wire w;
    bb.MakeWire(w);
    for (int k = 0; k < 3; ++k) bb.Add(w, side(c[k], c[(k + 1) % 3]));
    w.Closed(Standard_True);
    // A plane face needs no curves on its surface (OpenCASCADE projects them when asked).
    TopoDS_Face f;
    bb.MakeFace(f, new Geom_Plane(p[c[0]], gp_Dir(gp_Vec(p[c[0]], p[c[1]]).Crossed(gp_Vec(p[c[0]], p[c[2]])))), tol);
    bb.Add(f, w);
    bb.Add(shells[piece[i]], f);
  }
  TopoDS_Compound all;
  bb.MakeCompound(all);
  int open = 0;
  for (int k = 0; k < pieces; ++k) {
    shells[k].Closed(shut[k] != 0);
    if (!solid) {
      bb.Add(all, shells[k]);
    } else if (!shut[k]) {
      ++open;
    } else {
      TopoDS_Solid so;
      bb.MakeSolid(so);
      bb.Add(so, shells[k]);
      bb.Add(all, so);
    }
  }
  if (solid && open > 0)
    fail("the surfaces don't close up into a watertight shape (" + std::to_string(open) + " open pieces)");
  TopoDS_Shape r = all;
  // Neighbours in one plane merged, when there are any (a curved mesh has none, and looking for
  // them face by face takes seconds).
  auto unit = [&](size_t i) {
    gp_Vec n = gp_Vec(p[t[i][0]], p[t[i][1]]).Crossed(gp_Vec(p[t[i][0]], p[t[i][2]]));
    return n / n.Magnitude();
  };
  bool flat = false;
  for (const auto &kv : round)
    if (kv.second.size() == 2 && unit(kv.second[0]).Dot(unit(kv.second[1])) > 1 - 1e-12) {
      flat = true;
      break;
    }
  if (merge && flat) r = unified(r, TopoDS_Shape());
  // Made sound by construction: not checked face by face (BRepCheck takes seconds on a big mesh).
  return write(unwrap(r));
}

rust::String to_surface(const ShapeIn &in) {
  TopoDS_Shape s = placed(in);
  if (!has(s, TopAbs_SOLID)) return finish(s, "making a surface");
  BRep_Builder bb;
  TopoDS_Compound comp;
  bb.MakeCompound(comp);
  for (TopExp_Explorer ex(s, TopAbs_SHELL); ex.More(); ex.Next()) bb.Add(comp, ex.Current());
  return finish(comp, "making a surface");
}

rust::Vec<rust::String> split(const ShapeIn &in, rust::Slice<const ShapeIn> tools) {
  TopoDS_Shape s = placed(in);
  if (tools.empty()) fail("nothing to split with");
  TopTools_ListOfShape args, with;
  args.Append(s);
  for (const ShapeIn &t : tools) with.Append(placed(t));
  BRepAlgoAPI_Splitter sp;
  sp.SetArguments(args);
  sp.SetTools(with);
  sp.Build();
  if (!sp.IsDone() || sp.HasErrors()) fail("splitting failed");
  rust::Vec<rust::String> out;
  if (has(s, TopAbs_SOLID)) {
    for (TopExp_Explorer ex(sp.Shape(), TopAbs_SOLID); ex.More(); ex.Next()) out.push_back(write(ex.Current()));
    return out;
  }
  // A surface: its faces in pieces that hang together without crossing the cut (a face joins
  // its neighbour over an edge that isn't on a tool).
  TopoDS_Shape r = sp.Shape();
  TopTools_IndexedMapOfShape faces;
  TopExp::MapShapes(r, TopAbs_FACE, faces);
  TopTools_IndexedDataMapOfShapeListOfShape round;
  TopExp::MapShapesAndAncestors(r, TopAbs_EDGE, TopAbs_FACE, round);
  double tol = std::max(1e-6, diagonal(r) * 1e-7);
  std::vector<int> group(faces.Extent() + 1);
  for (int i = 1; i <= faces.Extent(); ++i) group[i] = i;
  std::function<int(int)> top = [&](int i) { return group[i] == i ? i : group[i] = top(group[i]); };
  for (int i = 1; i <= round.Extent(); ++i) {
    const TopTools_ListOfShape &fs = round(i);
    if (fs.Extent() < 2) continue;
    const TopoDS_Edge &e = TopoDS::Edge(round.FindKey(i));
    BRepAdaptor_Curve c(e);
    TopoDS_Vertex mid = BRepBuilderAPI_MakeVertex(c.Value((c.FirstParameter() + c.LastParameter()) / 2));
    bool cut = false;
    for (TopTools_ListIteratorOfListOfShape t(with); t.More() && !cut; t.Next()) {
      BRepExtrema_DistShapeShape d(mid, t.Value());
      cut = d.IsDone() && d.Value() < tol;
    }
    if (cut) continue;
    int a = top(faces.FindIndex(fs.First()));
    for (TopTools_ListIteratorOfListOfShape f(fs); f.More(); f.Next()) group[top(faces.FindIndex(f.Value()))] = a;
  }
  std::map<int, TopoDS_Shell> pieces;
  BRep_Builder bb;
  for (int i = 1; i <= faces.Extent(); ++i) {
    auto it = pieces.find(top(i));
    if (it == pieces.end()) {
      it = pieces.emplace(top(i), TopoDS_Shell()).first;
      bb.MakeShell(it->second);
    }
    bb.Add(it->second, faces(i));
  }
  for (auto &kv : pieces) out.push_back(write(unwrap(kv.second)));
  return out;
}

// --- surfaces from curves ------------------------------------------------------------------

rust::String ruled(const WireIn &a, const WireIn &b) {
  BRepOffsetAPI_ThruSections ts(Standard_False, Standard_True, 1e-6);
  ts.AddWire(wire_of(a));
  ts.AddWire(wire_of(b));
  ts.CheckCompatibility(Standard_False);
  ts.Build();
  if (!ts.IsDone()) fail("no ruled surface joins these curves");
  return finish(ts.Shape(), "the ruled surface");
}

rust::String tabulated(const WireIn &w, rust::Slice<const double> along) {
  gp_Vec v(pnt(along).XYZ());
  if (v.Magnitude() < 1e-9) fail("the direction is zero");
  BRepPrimAPI_MakePrism pr(wire_of(w), v);
  return finish(pr.Shape(), "the tabulated surface");
}

rust::String revolved(const WireIn &w, rust::Slice<const double> origin, rust::Slice<const double> axis, double start,
                      double angle) {
  gp_Ax1 ax(pnt(origin), dir(axis));
  TopoDS_Shape c = wire_of(w);
  if (std::abs(start) > 1e-12) {
    gp_Trsf t;
    t.SetRotation(ax, start);
    c = moved(c, t);
  }
  if (std::abs(angle) >= 2 * M_PI - 1e-9) return finish(BRepPrimAPI_MakeRevol(c, ax).Shape(), "the revolved surface");
  return finish(BRepPrimAPI_MakeRevol(c, ax, angle).Shape(), "the revolved surface");
}

namespace {
// The curve as one B-spline, its pieces in the order they run.
Handle(Geom_BSplineCurve) bspline_of(const WireIn &in) {
  TopoDS_Wire w = wire_of(in);
  GeomConvert_CompCurveToBSplineCurve cc;
  for (BRepTools_WireExplorer ex(w); ex.More(); ex.Next()) {
    const TopoDS_Edge &e = ex.Current();
    double f, l;
    Handle(Geom_Curve) g = BRep_Tool::Curve(e, f, l);
    if (g.IsNull()) fail("an edge curve has no shape");
    Handle(Geom_BSplineCurve) piece = GeomConvert::CurveToBSplineCurve(new Geom_TrimmedCurve(g, f, l));
    if (e.Orientation() == TopAbs_REVERSED) piece->Reverse();
    if (!cc.Add(piece, 1e-6)) fail("the pieces of an edge curve must meet end to end");
  }
  Handle(Geom_BSplineCurve) c = cc.BSplineCurve();
  if (c.IsNull()) fail("an edge curve has no pieces");
  // A Coons patch needs four poles a side.
  if (c->Degree() < 3) c->IncreaseDegree(3);
  return c;
}
} // namespace

rust::String coons(rust::Slice<const WireIn> edges) {
  if (edges.size() != 4) fail("an edge surface needs four edges");
  Handle(Geom_BSplineCurve) c[4];
  for (size_t i = 0; i < 4; ++i) c[i] = bspline_of(edges[i]);
  GeomFill_BSplineCurves fill(c[0], c[1], c[2], c[3], GeomFill_CoonsStyle);
  const Handle(Geom_BSplineSurface) &srf = fill.Surface();
  if (srf.IsNull()) fail("the four edges don't make a closed loop");
  return finish(BRepBuilderAPI_MakeFace(srf, 1e-6).Face(), "the edge surface");
}

rust::String planar_face(const WireIn &outer, rust::Slice<const WireIn> holes) {
  return finish(face_of(outer, holes), "the planar surface");
}

rust::String pipe_loft(rust::Slice<const WireIn> sections, const WireIn &spine, bool solid) {
  if (sections.empty()) fail("a loft needs cross sections");
  BRepOffsetAPI_MakePipeShell ps(wire_of(spine));
  for (const WireIn &s : sections) ps.Add(wire_of(s), Standard_False, Standard_False);
  ps.Build();
  if (!ps.IsDone()) fail("no loft runs through these sections along that curve");
  if (solid && !ps.MakeSolid()) fail("the loft can't be closed into a solid");
  return finish(ps.Shape(), "lofting");
}

namespace {
// A cross section as pieces of curve from corner to corner, in the order they run: a closed one
// has as many corners as pieces (piece k ends where piece k + 1 starts), an open one one more.
struct Ring {
  std::vector<Handle(Geom_BSplineCurve)> pieces;
  bool closed = false;
  gp_Pnt corner(size_t k) const {
    if (k < pieces.size()) return pieces[k]->StartPoint();
    return pieces.back()->EndPoint();
  }
  size_t corners() const { return closed ? pieces.size() : pieces.size() + 1; }
};

Ring ring_of(const WireIn &in) {
  TopoDS_Wire w = wire_of(in);
  Ring r;
  for (BRepTools_WireExplorer ex(w); ex.More(); ex.Next()) {
    const TopoDS_Edge &e = ex.Current();
    double f, l;
    Handle(Geom_Curve) g = BRep_Tool::Curve(e, f, l);
    if (g.IsNull()) fail("a cross section has a piece with no shape");
    Handle(Geom_BSplineCurve) piece = GeomConvert::CurveToBSplineCurve(new Geom_TrimmedCurve(g, f, l));
    if (e.Orientation() == TopAbs_REVERSED) piece->Reverse();
    r.pieces.push_back(piece);
  }
  if (r.pieces.empty()) fail("a cross section has no pieces");
  r.closed = r.pieces.front()->StartPoint().Distance(r.pieces.back()->EndPoint()) < 1e-6;
  return r;
}

// The corner at `p` on the section (made one by splitting the piece it lies on): its index.
size_t corner_at(Ring &r, const gp_Pnt &p, double tol) {
  for (size_t k = 0; k < r.corners(); ++k)
    if (r.corner(k).Distance(p) < tol) return k;
  for (size_t k = 0; k < r.pieces.size(); ++k) {
    GeomAPI_ProjectPointOnCurve pr(p, r.pieces[k]);
    if (pr.NbPoints() == 0 || pr.LowerDistance() > tol) continue;
    double u = pr.LowerDistanceParameter(), u0 = r.pieces[k]->FirstParameter(), u1 = r.pieces[k]->LastParameter();
    Handle(Geom_BSplineCurve) a = GeomConvert::SplitBSplineCurve(r.pieces[k], u0, u, 1e-9);
    Handle(Geom_BSplineCurve) b = GeomConvert::SplitBSplineCurve(r.pieces[k], u, u1, 1e-9);
    r.pieces[k] = a;
    r.pieces.insert(r.pieces.begin() + static_cast<long>(k) + 1, b);
    return k + 1;
  }
  fail("a guide curve must touch every cross section");
}

void reverse(Ring &r) {
  std::reverse(r.pieces.begin(), r.pieces.end());
  for (Handle(Geom_BSplineCurve) &c : r.pieces) {
    c = Handle(Geom_BSplineCurve)::DownCast(c->Copy());
    c->Reverse();
  }
}

// The way round a closed section runs (Newell's normal of points along it).
gp_XYZ ring_normal(const Ring &r) {
  std::vector<gp_Pnt> pts;
  for (const Handle(Geom_BSplineCurve) &c : r.pieces)
    for (int i = 0; i < 8; ++i) pts.push_back(c->Value(c->FirstParameter() + (c->LastParameter() - c->FirstParameter()) * i / 8));
  gp_XYZ n(0, 0, 0);
  for (size_t i = 0; i < pts.size(); ++i) n += pts[i].XYZ().Crossed(pts[(i + 1) % pts.size()].XYZ());
  return n;
}

// A Coons patch needs four poles a side.
Handle(Geom_BSplineCurve) cubic(const Handle(Geom_BSplineCurve) &c) {
  Handle(Geom_BSplineCurve) out = Handle(Geom_BSplineCurve)::DownCast(c->Copy());
  if (out->Degree() < 3) out->IncreaseDegree(3);
  return out;
}
} // namespace

// LOFT Guides as AutoCAD's: each guide runs through a corner of every section (one is made where
// it touches), and the loft is a patch for each piece of each span between two sections, bounded
// by the two section pieces and the curves through their corners from section to section: a
// guide where there is one, else straight on (two sections) or smoothly through the corners. So
// a guide shapes only the faces it bounds and the far side of the loft stays as it was.
// ponytail: the patches meet edge to edge, not smoothly across a guide or a corner; a Gordon
// surface over the whole curve network if smooth guided lofts are needed.
rust::String guided_loft(rust::Slice<const WireIn> sections, rust::Slice<const WireIn> guides, bool solid) {
  if (sections.size() < 2) fail("a guided loft needs two cross sections or more");
  if (guides.empty()) fail("a guided loft needs a guide curve");
  size_t m = sections.size();
  std::vector<Ring> rings;
  Bnd_Box b;
  for (const WireIn &w : sections) {
    rings.push_back(ring_of(w));
    BRepBndLib::Add(wire_of(w), b);
  }
  double tol = std::max(1e-6, std::sqrt(b.SquareExtent()) * 1e-6);
  // Each guide as one curve, where it touches each section, run from the first to the last.
  std::vector<Handle(Geom_BSplineCurve)> g;
  std::vector<std::vector<gp_Pnt>> touch(guides.size(), std::vector<gp_Pnt>(m));
  std::vector<std::vector<double>> at(guides.size(), std::vector<double>(m));
  for (size_t j = 0; j < guides.size(); ++j) {
    g.push_back(bspline_of(guides[j]));
    TopoDS_Edge ge = BRepBuilderAPI_MakeEdge(g[j]).Edge();
    for (size_t i = 0; i < m; ++i) {
      BRepExtrema_DistShapeShape d(ge, wire_of(sections[i]));
      if (!d.IsDone() || d.NbSolution() == 0 || d.Value() > tol) fail("a guide curve must touch every cross section");
      touch[j][i] = d.PointOnShape2(1);
      GeomAPI_ProjectPointOnCurve pr(touch[j][i], g[j]);
      if (pr.NbPoints() == 0) fail("a guide curve must touch every cross section");
      at[j][i] = pr.LowerDistanceParameter();
    }
    if (at[j][0] > at[j][m - 1]) {
      double f = g[j]->FirstParameter(), l = g[j]->LastParameter();
      g[j]->Reverse();
      for (double &u : at[j]) u = f + l - u;
    }
    for (size_t i = 1; i < m; ++i)
      if (at[j][i] <= at[j][i - 1]) fail("a guide curve must run from the first cross section to the last, through each in turn");
  }
  // Each section turned the same way round as the first, with a corner at each guide, a closed
  // one starting at the first guide's.
  for (size_t i = 0; i < m; ++i) {
    Ring &r = rings[i];
    if (r.closed != rings[0].closed) fail("cross sections must be all open or all closed");
    if (i > 0) {
      const Ring &r0 = rings[0];
      size_t e = r.corners() - 1, e0 = r0.corners() - 1;
      bool turn = r.closed ? ring_normal(r).Dot(ring_normal(r0)) < 0
                           : r.corner(0).Distance(r0.corner(0)) + r.corner(e).Distance(r0.corner(e0)) >
                                 r.corner(0).Distance(r0.corner(e0)) + r.corner(e).Distance(r0.corner(0));
      if (turn) reverse(r);
    }
    for (size_t j = 0; j < guides.size(); ++j) corner_at(r, touch[j][i], tol);
    if (r.closed) {
      size_t k0 = corner_at(r, touch[0][i], tol);
      std::rotate(r.pieces.begin(), r.pieces.begin() + static_cast<long>(k0), r.pieces.end());
    }
  }
  size_t n = rings[0].pieces.size();
  for (const Ring &r : rings)
    if (r.pieces.size() != n) fail("with guide curves, each cross section needs as many corners as the others");
  // The curve through each corner from section to section, span by span.
  size_t corners = rings[0].corners();
  std::vector<std::vector<Handle(Geom_BSplineCurve)>> rails(corners);
  for (size_t k = 0; k < corners; ++k) {
    size_t guide = guides.size();
    for (size_t j = 0; j < guides.size(); ++j)
      if (rings[0].corner(k).Distance(touch[j][0]) < tol) guide = j;
    if (guide < guides.size()) {
      for (size_t i = 0; i < m; ++i)
        if (rings[i].corner(k).Distance(touch[guide][i]) > tol) fail("each guide curve must run through matching corners of the cross sections");
      for (size_t i = 0; i + 1 < m; ++i) rails[k].push_back(GeomConvert::SplitBSplineCurve(g[guide], at[guide][i], at[guide][i + 1], 1e-9));
      continue;
    }
    if (m == 2) {
      gp_Pnt a = rings[0].corner(k), c = rings[1].corner(k);
      if (a.Distance(c) < tol) fail("two cross sections meet at a corner");
      Handle(Geom_Curve) line = new Geom_Line(a, gp_Dir(gp_Vec(a, c)));
      rails[k].push_back(GeomConvert::CurveToBSplineCurve(new Geom_TrimmedCurve(line, 0, a.Distance(c))));
      continue;
    }
    Handle(TColgp_HArray1OfPnt) pts = new TColgp_HArray1OfPnt(1, static_cast<int>(m));
    for (size_t i = 0; i < m; ++i) pts->SetValue(static_cast<int>(i) + 1, rings[i].corner(k));
    GeomAPI_Interpolate ip(pts, Standard_False, 1e-7);
    ip.Perform();
    if (!ip.IsDone()) fail("the cross sections' corners can't be joined smoothly");
    Handle(Geom_BSplineCurve) c = ip.Curve();
    std::vector<double> u(m);
    for (size_t i = 0; i < m; ++i) {
      GeomAPI_ProjectPointOnCurve pr(rings[i].corner(k), c);
      u[i] = pr.NbPoints() > 0 ? pr.LowerDistanceParameter() : c->FirstParameter();
    }
    for (size_t i = 0; i + 1 < m; ++i) rails[k].push_back(GeomConvert::SplitBSplineCurve(c, u[i], u[i + 1], 1e-9));
  }
  BRepBuilderAPI_Sewing sw(std::max(1e-6, tol * 10));
  for (size_t i = 0; i + 1 < m; ++i) {
    for (size_t k = 0; k < n; ++k) {
      size_t k2 = (k + 1) % corners;
      GeomFill_BSplineCurves fill(cubic(rings[i].pieces[k]), cubic(rails[k2][i]), cubic(rings[i + 1].pieces[k]), cubic(rails[k][i]),
                                  GeomFill_CoonsStyle);
      const Handle(Geom_BSplineSurface) &srf = fill.Surface();
      if (srf.IsNull()) fail("the loft can't be made between these cross sections and guides");
      sw.Add(BRepBuilderAPI_MakeFace(srf, 1e-6).Face());
    }
  }
  if (solid) {
    sw.Add(face_of(sections[0], rust::Slice<const WireIn>()));
    sw.Add(face_of(sections[m - 1], rust::Slice<const WireIn>()));
  }
  sw.Perform();
  TopoDS_Shape r = sw.SewedShape();
  return finish(solid ? solids_of(r) : r, "lofting");
}

// --- booleans and exchange files -------------------------------------------------------------

// With `merge`, faces left on one surface merged (two touching boxes joined have six faces, not
// ten), edges either solid had between faces on one surface (imprints) kept, and a result that
// isn't valid refused; without, the boolean's own result, healed where it can be, else as it is.
rust::String boolean(const ShapeIn &ina, const ShapeIn &inb, uint8_t op, bool merge) {
  TopoDS_Shape a = placed(ina), b = placed(inb), r;
  if (op == 0) {
    r = fused(a, b);
  } else if (op == 1) {
    r = cut(a, b);
  } else {
    BRepAlgoAPI_Common c(a, b);
    if (!c.IsDone()) fail("intersecting the solids failed");
    r = c.Shape();
  }
  if (!has(r, TopAbs_FACE)) fail("the result is empty");
  if (!merge) {
    TopoDS_Shape s = healed(r);
    return write(s.IsNull() ? unwrap(r) : s);
  }
  BRep_Builder bb;
  TopoDS_Compound both;
  bb.MakeCompound(both);
  bb.Add(both, a);
  bb.Add(both, b);
  return finish(unified(r, both), "combining solids");
}

namespace {
// One object of an exchange file as other modellers take it whole: a solid as it is; faces outside
// a solid sewn, closed pieces made solids and open ones shells (IGES's BRep mode writes a lone
// face as a face entity, not a surface); curves as they are.
TopoDS_Shape exchanged(const TopoDS_Shape &shape) {
  TopoDS_Shape s = unwrap(shape);
  if (!has(s, TopAbs_FACE) || has(s, TopAbs_SOLID)) return s;
  BRepBuilderAPI_Sewing sw(std::max(1e-6, diagonal(s) * 1e-7));
  sw.Add(s);
  sw.Perform();
  TopoDS_Shape r = sw.SewedShape();
  BRep_Builder bb;
  TopoDS_Compound out;
  bb.MakeCompound(out);
  for (TopExp_Explorer ex(r, TopAbs_SHELL); ex.More(); ex.Next()) {
    TopoDS_Shell sh = TopoDS::Shell(ex.Current());
    if (!BRep_Tool::IsClosed(sh)) {
      bb.Add(out, sh);
      continue;
    }
    TopoDS_Solid so = BRepBuilderAPI_MakeSolid(sh).Solid();
    BRepLib::OrientClosedSolid(so);
    bb.Add(out, so);
  }
  for (TopExp_Explorer ex(r, TopAbs_FACE, TopAbs_SHELL); ex.More(); ex.Next()) {
    TopoDS_Shell sh;
    bb.MakeShell(sh);
    bb.Add(sh, ex.Current());
    bb.Add(out, sh);
  }
  return unwrap(out);
}

TCollection_ExtendedString text_of(const rust::String &s) {
  return TCollection_ExtendedString(std::string(s).c_str(), Standard_True);
}

// IGES BRep mode writes every shell as a closed one (form 1); those under no solid are open.
void open_shells(const Handle(IGESData_IGESModel) &model) {
  std::vector<Handle(IGESSolid_Shell)> closed, all;
  for (int i = 1; i <= model->NbEntities(); ++i) {
    Handle(Standard_Transient) e = model->Value(i);
    if (Handle(IGESSolid_ManifoldSolid) so = Handle(IGESSolid_ManifoldSolid)::DownCast(e)) {
      closed.push_back(so->Shell());
      for (int v = 1; v <= so->NbVoidShells(); ++v) closed.push_back(so->VoidShell(v));
    } else if (Handle(IGESSolid_Shell) sh = Handle(IGESSolid_Shell)::DownCast(e)) {
      all.push_back(sh);
    }
  }
  for (const Handle(IGESSolid_Shell) &sh : all)
    if (std::find(closed.begin(), closed.end(), sh) == closed.end()) sh->SetClosed(Standard_False);
}

// STEP text keeps to the basic alphabet: OpenCASCADE writes names and layers as UTF-8, so each
// run of other characters becomes \X2\, 4 hex digits a character (\X4\ and 8 past U+FFFF) and
// \X0\, as ISO 10303-21 has it (only quoted strings can hold such characters).
std::string step_text(const std::string &in) {
  const char *const opens[] = {"", "", "\\X2\\", "", "\\X4\\"};
  std::string out;
  int mode = 0;
  for (size_t i = 0; i < in.size();) {
    unsigned char c = static_cast<unsigned char>(in[i]);
    size_t len = c < 0x80 ? 1 : c >= 0xF0 ? 4 : c >= 0xE0 ? 3 : c >= 0xC0 ? 2 : 1;
    uint32_t cp = len == 4 ? c & 7u : len == 3 ? c & 15u : len == 2 ? c & 31u : c < 0x80 ? c : 0xFFFDu;
    for (size_t k = 1; k < len; ++k) {
      unsigned char d = i + k < in.size() ? static_cast<unsigned char>(in[i + k]) : 0;
      if ((d & 0xC0) != 0x80) {
        cp = 0xFFFD;
        len = k;
        break;
      }
      cp = cp << 6 | (d & 63u);
    }
    i += len;
    int want = cp < 0x80 ? 0 : cp > 0xFFFF ? 4 : 2;
    if (want != mode) out += std::string(mode != 0 ? "\\X0\\" : "") + opens[want];
    mode = want;
    if (want == 0) {
      out += static_cast<char>(cp);
      continue;
    }
    char hex[9];
    std::snprintf(hex, sizeof hex, want == 2 ? "%04X" : "%08X", static_cast<unsigned>(cp));
    out += hex;
  }
  if (mode != 0) out += "\\X0\\";
  return out;
}
} // namespace

// An XDE document of the objects (each its own object, named, coloured, on its layer) and curves,
// written as IGES in BRep mode (a solid is one closed object, not a surface per face) or as STEP,
// in millimetres.
rust::String exchange(rust::Slice<const ObjectIn> objects, rust::Slice<const WireIn> curves, bool iges) {
  Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
  XCAFDoc_DocumentTool::Set(doc->Main(), Standard_False);
  XCAFDoc_DocumentTool::SetLengthUnit(doc, 0.001);
  Handle(XCAFDoc_ShapeTool) shapes = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
  Handle(XCAFDoc_ColorTool) colors = XCAFDoc_DocumentTool::ColorTool(doc->Main());
  Handle(XCAFDoc_LayerTool) layers = XCAFDoc_DocumentTool::LayerTool(doc->Main());
  for (const ObjectIn &o : objects) {
    TDF_Label l = shapes->AddShape(exchanged(placed(o.shape)), Standard_False);
    if (!o.name.empty()) TDataStd_Name::Set(l, text_of(o.name));
    if (!o.layer.empty()) layers->SetLayer(l, text_of(o.layer));
    if (o.color[0] >= 0)
      colors->SetColor(l, Quantity_Color(o.color[0], o.color[1], o.color[2], Quantity_TOC_sRGB), XCAFDoc_ColorGen);
  }
  for (const WireIn &w : curves) shapes->AddShape(wire_of(w), Standard_False);
  std::ostringstream out;
  if (iges) {
    // The writer takes its mode when made; opencascade-rs's writes faces mode, so it is put back.
    IGESControl_Controller::Init();
    int mode = Interface_Static::IVal("write.iges.brep.mode");
    Interface_Static::SetIVal("write.iges.brep.mode", 1);
    IGESCAFControl_Writer w;
    Interface_Static::SetIVal("write.iges.brep.mode", mode);
    if (!w.Transfer(doc)) fail("writing IGES failed");
    open_shells(w.Model());
    if (!w.Write(out)) fail("writing IGES failed");
    return rust::String(out.str());
  }
  STEPCAFControl_Writer w;
  if (!w.Transfer(doc, STEPControl_AsIs) || w.WriteStream(out) != IFSelect_RetDone) fail("writing STEP failed");
  return rust::String(step_text(out.str()));
}

} // namespace cadrhino

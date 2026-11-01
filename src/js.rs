//! JavaScript execution via Boa, bound to the mutable DOM mirror in `dom.rs`.
//!
//! Design notes:
//! * Scripts run **before** the immutable render pipeline: the fetched HTML is
//!   mirrored into `dom::Doc`, every classic `<script>` (inline or fetched
//!   `src`) is evaluated in document order in one Boa context, and the mutated
//!   mirror is serialized back to HTML for the normal parse → layout → paint.
//! * DOM handles handed to JS are plain objects carrying a hidden
//!   `__ferris_node` index into the mirror arena; every method is a native
//!   function that looks the node up again. The mirror itself is pure Rust,
//!   so this whole binding layer is swappable without touching the engine.
//! * Runaway scripts are contained with Boa's loop-iteration and recursion
//!   limits; errors are captured per page instead of killing the render.
//! * Only available with `--features js`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use boa_engine::object::builtins::{JsArray, JsFunction};
use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsResult, JsValue, NativeFunction, Source, js_string,
};
use url::Url;

use crate::dom::Doc;
use crate::engine::JsReport;

/// Hard caps so a hostile or buggy page cannot wedge the browser.
const MAX_SCRIPT_BYTES: usize = 6 * 1024 * 1024;
const MAX_CONSOLE_LINES: usize = 200;
const MAX_ERROR_LINES: usize = 40;
const LOOP_ITERATION_LIMIT: u64 = 5_000_000;
const RECURSION_LIMIT: usize = 128;
const SCRIPT_FETCH_BYTES: usize = 2 * 1024 * 1024;

pub struct World {
    pub doc: Doc,
    pub base: Url,
    pub console: Vec<String>,
    pub errors: Vec<String>,
    pub scripts: usize,
}

thread_local! {
    static WORLD: RefCell<Option<Rc<RefCell<World>>>> = const { RefCell::new(None) };
}

fn world() -> Rc<RefCell<World>> {
    WORLD
        .with(|w| w.borrow().clone())
        .expect("ferris: DOM world not initialized")
}

fn with_doc<F: FnOnce(&mut Doc) -> R, R>(f: F) -> R {
    f(&mut world().borrow_mut().doc)
}

type Nfn = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn throw(msg: &str) -> JsError {
    JsNativeError::typ().with_message(msg.to_string()).into()
}

fn arg_str(args: &[JsValue], ctx: &mut Context, i: usize) -> JsResult<String> {
    Ok(args
        .get_or_undefined(i)
        .to_string(ctx)?
        .to_std_string_escaped())
}

fn node_id(this: &JsValue, ctx: &mut Context) -> JsResult<usize> {
    let obj = this
        .as_object()
        .ok_or_else(|| throw("not a Ferris DOM node"))?;
    let v = obj.get(js_string!("__ferris_node"), ctx)?;
    v.as_number()
        .map(|n| n as usize)
        .ok_or_else(|| throw("not a Ferris DOM node"))
}

fn data(v: impl Into<JsValue>) -> PropertyDescriptor {
    PropertyDescriptor::builder()
        .value(v)
        .writable(true)
        .enumerable(true)
        .configurable(true)
        .build()
}

fn ro(v: impl Into<JsValue>) -> PropertyDescriptor {
    PropertyDescriptor::builder()
        .value(v)
        .writable(false)
        .enumerable(true)
        .configurable(true)
        .build()
}

fn method(ctx: &mut Context, name: &str, len: usize, f: Nfn) -> PropertyDescriptor {
    let f = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(f))
        .name(js_string!(name))
        .length(len)
        .build();
    data(JsValue::from(f))
}

fn accessor(ctx: &mut Context, get: Nfn, set: Option<Nfn>) -> PropertyDescriptor {
    let g = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(get)).build();
    let mut b = PropertyDescriptor::builder()
        .get(JsValue::from(g))
        .configurable(true)
        .enumerable(true);
    if let Some(s) = set {
        let s = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(s)).build();
        b = b.set(JsValue::from(s));
    }
    b.build()
}

// ---------------------------------------------------------------------------
// Element methods
// ---------------------------------------------------------------------------

fn el_append_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let parent = node_id(this, ctx)?;
    let child = node_id(args.get_or_undefined(0), ctx)?;
    with_doc(|d| d.append(parent, child));
    Ok(args.get_or_undefined(0).clone())
}

fn el_remove_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let child = node_id(args.get_or_undefined(0), ctx)?;
    let parent = node_id(this, ctx)?;
    let ok = with_doc(|d| d.nodes.get(child).map(|n| n.parent) == Some(Some(parent)));
    if ok {
        with_doc(|d| d.remove(child));
        Ok(args.get_or_undefined(0).clone())
    } else {
        Err(throw("removeChild: node is not a child"))
    }
}

fn el_remove(this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    with_doc(|d| d.remove(i));
    Ok(JsValue::undefined())
}

fn el_set_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let k = arg_str(args, ctx, 0)?;
    let v = arg_str(args, ctx, 1)?;
    with_doc(|d| d.set_attr(i, &k.to_ascii_lowercase(), &v));
    Ok(JsValue::undefined())
}

fn el_get_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let k = arg_str(args, ctx, 0)?.to_ascii_lowercase();
    Ok(with_doc(|d| {
        d.attr(i, &k)
            .map(|v| JsValue::from(js_string!(&*v.to_string())))
    })
    .unwrap_or(JsValue::null()))
}

fn el_remove_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let k = arg_str(args, ctx, 0)?.to_ascii_lowercase();
    with_doc(|d| d.remove_attr(i, &k));
    Ok(JsValue::undefined())
}

fn el_has_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let k = arg_str(args, ctx, 0)?.to_ascii_lowercase();
    Ok(JsValue::from(with_doc(|d| d.attr(i, &k).is_some())))
}

fn el_query_selector(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let sel = arg_str(args, ctx, 0)?;
    let hit = with_doc(|d| d.match_simple(i, &sel).first().copied());
    Ok(match hit {
        Some(h) => JsValue::from(element_obj(ctx, h)),
        None => JsValue::null(),
    })
}

fn el_query_selector_all(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let sel = arg_str(args, ctx, 0)?;
    let hits = with_doc(|d| d.match_simple(i, &sel));
    Ok(JsValue::from(nodes_to_array(ctx, hits)))
}

fn el_get_elements_by_tag(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let tag = arg_str(args, ctx, 0)?.to_ascii_lowercase();
    let hits = with_doc(|d| {
        d.elements_under(i)
            .into_iter()
            .filter(|n| *n != i && d.tag(*n) == Some(tag.as_str()))
            .collect::<Vec<_>>()
    });
    Ok(JsValue::from(nodes_to_array(ctx, hits)))
}

fn el_get_elements_by_class(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let class = arg_str(args, ctx, 0)?;
    let hits = with_doc(|d| {
        d.elements_under(i)
            .into_iter()
            .filter(|n| {
                *n != i
                    && d.attr(*n, "class")
                        .map(|c| c.split_whitespace().any(|c| c == class))
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>()
    });
    Ok(JsValue::from(nodes_to_array(ctx, hits)))
}

fn el_text_content_get(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(js_string!(&*with_doc(|d| d.text_content(i)))))
}

fn el_text_content_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let t = arg_str(args, ctx, 0)?;
    with_doc(|d| d.set_text_content(i, &t));
    Ok(JsValue::undefined())
}

fn el_inner_html_get(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(js_string!(&*with_doc(|d| d.inner_html(i)))))
}

fn el_inner_html_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let h = arg_str(args, ctx, 0)?;
    with_doc(|d| d.set_inner_html(i, &h));
    Ok(JsValue::undefined())
}

fn el_id_get(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(js_string!(&*with_doc(|d| d
        .attr(i, "id")
        .unwrap_or("")
        .to_string()))))
}

fn el_id_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let v = arg_str(args, ctx, 0)?;
    with_doc(|d| d.set_attr(i, "id", &v));
    Ok(JsValue::undefined())
}

fn el_class_get(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(js_string!(&*with_doc(|d| d
        .attr(i, "class")
        .unwrap_or("")
        .to_string()))))
}

fn el_class_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let v = arg_str(args, ctx, 0)?;
    with_doc(|d| d.set_attr(i, "class", &v));
    Ok(JsValue::undefined())
}

fn el_class_list(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(class_list_obj(ctx, i)))
}

fn el_tag_name(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(js_string!(&*with_doc(|d| d
        .tag(i)
        .unwrap_or("")
        .to_ascii_uppercase()))))
}

fn el_parent_element(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let p = with_doc(|d| {
        let mut p = d.nodes.get(i).and_then(|n| n.parent);
        while let Some(pp) = p {
            if d.tag(pp).is_some() {
                return Some(pp);
            }
            p = d.nodes.get(pp).and_then(|n| n.parent);
        }
        None
    });
    Ok(match p {
        Some(p) => JsValue::from(element_obj(ctx, p)),
        None => JsValue::null(),
    })
}

fn el_children(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let kids = with_doc(|d| {
        d.nodes[i]
            .children
            .iter()
            .copied()
            .filter(|c| d.tag(*c).is_some())
            .collect::<Vec<_>>()
    });
    Ok(JsValue::from(nodes_to_array(ctx, kids)))
}

fn el_style(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    Ok(JsValue::from(style_obj(ctx, i)))
}

fn el_add_event_listener(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<JsValue> {
    // No event loop yet: listeners are accepted and ignored.
    Ok(JsValue::undefined())
}

fn el_to_string(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let s = with_doc(|d| {
        format!(
            "[object HTML{}Element]",
            d.tag(i).unwrap_or("Unknown").to_ascii_uppercase()
        )
    });
    Ok(JsValue::from(js_string!(&*s)))
}

/// Mirror node ids → JS array of element objects (built eagerly: the array
/// and the element factory both need `&mut Context`, so no lazy iterators).
fn nodes_to_array(ctx: &mut Context, ids: Vec<usize>) -> JsArray {
    let arr = JsArray::new(ctx).expect("ferris: array allocation");
    for i in ids {
        let _ = arr.push(JsValue::from(element_obj(ctx, i)), ctx);
    }
    arr
}

/// Build a JS object wrapping mirror node `i`.
fn element_obj(ctx: &mut Context, i: usize) -> JsObject {
    let o = JsObject::with_object_proto(ctx.intrinsics());
    o.insert_property(js_string!("__ferris_node"), ro(JsValue::from(i as f64)));
    o.insert_property(js_string!("nodeType"), ro(JsValue::from(1)));
    o.insert_property(
        js_string!("appendChild"),
        method(ctx, "appendChild", 1, el_append_child),
    );
    o.insert_property(
        js_string!("removeChild"),
        method(ctx, "removeChild", 1, el_remove_child),
    );
    o.insert_property(js_string!("remove"), method(ctx, "remove", 0, el_remove));
    o.insert_property(
        js_string!("setAttribute"),
        method(ctx, "setAttribute", 2, el_set_attribute),
    );
    o.insert_property(
        js_string!("getAttribute"),
        method(ctx, "getAttribute", 1, el_get_attribute),
    );
    o.insert_property(
        js_string!("removeAttribute"),
        method(ctx, "removeAttribute", 1, el_remove_attribute),
    );
    o.insert_property(
        js_string!("hasAttribute"),
        method(ctx, "hasAttribute", 1, el_has_attribute),
    );
    o.insert_property(
        js_string!("querySelector"),
        method(ctx, "querySelector", 1, el_query_selector),
    );
    o.insert_property(
        js_string!("querySelectorAll"),
        method(ctx, "querySelectorAll", 1, el_query_selector_all),
    );
    o.insert_property(
        js_string!("getElementsByTagName"),
        method(ctx, "getElementsByTagName", 1, el_get_elements_by_tag),
    );
    o.insert_property(
        js_string!("getElementsByClassName"),
        method(ctx, "getElementsByClassName", 1, el_get_elements_by_class),
    );
    o.insert_property(
        js_string!("addEventListener"),
        method(ctx, "addEventListener", 2, el_add_event_listener),
    );
    o.insert_property(
        js_string!("toString"),
        method(ctx, "toString", 0, el_to_string),
    );
    o.insert_property(
        js_string!("textContent"),
        accessor(ctx, el_text_content_get, Some(el_text_content_set)),
    );
    o.insert_property(
        js_string!("innerHTML"),
        accessor(ctx, el_inner_html_get, Some(el_inner_html_set)),
    );
    o.insert_property(js_string!("id"), accessor(ctx, el_id_get, Some(el_id_set)));
    o.insert_property(
        js_string!("className"),
        accessor(ctx, el_class_get, Some(el_class_set)),
    );
    o.insert_property(js_string!("classList"), accessor(ctx, el_class_list, None));
    o.insert_property(js_string!("tagName"), accessor(ctx, el_tag_name, None));
    o.insert_property(
        js_string!("parentElement"),
        accessor(ctx, el_parent_element, None),
    );
    o.insert_property(js_string!("children"), accessor(ctx, el_children, None));
    o.insert_property(js_string!("style"), accessor(ctx, el_style, None));
    o
}

// ---------------------------------------------------------------------------
// classList + style sub-objects
// ---------------------------------------------------------------------------

fn cl_add(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    for a in args {
        let c = a.to_string(ctx)?.to_std_string_escaped();
        with_doc(|d| {
            let mut cur: Vec<String> = d
                .attr(i, "class")
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_string)
                .collect();
            if !cur.contains(&c) {
                cur.push(c);
            }
            let joined = cur.join(" ");
            d.set_attr(i, "class", &joined);
        });
    }
    Ok(JsValue::undefined())
}

fn cl_remove(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    for a in args {
        let c = a.to_string(ctx)?.to_std_string_escaped();
        with_doc(|d| {
            let cur: Vec<String> = d
                .attr(i, "class")
                .unwrap_or("")
                .split_whitespace()
                .filter(|x| *x != c)
                .map(str::to_string)
                .collect();
            let joined = cur.join(" ");
            d.set_attr(i, "class", &joined);
        });
    }
    Ok(JsValue::undefined())
}

fn cl_contains(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let c = arg_str(args, ctx, 0)?;
    Ok(JsValue::from(with_doc(|d| {
        d.attr(i, "class")
            .map(|v| v.split_whitespace().any(|x| x == c))
            .unwrap_or(false)
    })))
}

fn cl_toggle(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let has = cl_contains(this, args, ctx)?;
    if has.as_boolean() == Some(true) {
        cl_remove(this, args, ctx)?;
        Ok(JsValue::from(false))
    } else {
        cl_add(this, args, ctx)?;
        Ok(JsValue::from(true))
    }
}

fn class_list_obj(ctx: &mut Context, i: usize) -> JsObject {
    let o = JsObject::with_object_proto(ctx.intrinsics());
    o.insert_property(js_string!("__ferris_node"), ro(JsValue::from(i as f64)));
    o.insert_property(js_string!("add"), method(ctx, "add", 1, cl_add));
    o.insert_property(js_string!("remove"), method(ctx, "remove", 1, cl_remove));
    o.insert_property(
        js_string!("contains"),
        method(ctx, "contains", 1, cl_contains),
    );
    o.insert_property(js_string!("toggle"), method(ctx, "toggle", 1, cl_toggle));
    o
}

fn style_set_property(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let k = arg_str(args, ctx, 0)?;
    let v = arg_str(args, ctx, 1)?;
    with_doc(|d| {
        let cur = d.attr(i, "style").unwrap_or("").to_string();
        let mut decls: Vec<(String, String)> = cur
            .split(';')
            .filter_map(|p| {
                let mut it = p.splitn(2, ':');
                match (it.next(), it.next()) {
                    (Some(k), Some(v)) if !k.trim().is_empty() => {
                        Some((k.trim().to_string(), v.trim().to_string()))
                    }
                    _ => None,
                }
            })
            .collect();
        decls.retain(|(dk, _)| dk != &k);
        if !v.is_empty() {
            decls.push((k, v));
        }
        let joined = decls
            .into_iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("; ");
        d.set_attr(i, "style", &joined);
    });
    Ok(JsValue::undefined())
}

fn style_get_property(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = node_id(this, ctx)?;
    let k = arg_str(args, ctx, 0)?;
    let v = with_doc(|d| {
        d.attr(i, "style")
            .unwrap_or("")
            .split(';')
            .find_map(|p| {
                let mut it = p.splitn(2, ':');
                match (it.next(), it.next()) {
                    (Some(dk), Some(dv)) if dk.trim() == k => Some(dv.trim().to_string()),
                    _ => None,
                }
            })
            .unwrap_or_default()
    });
    Ok(JsValue::from(js_string!(&*v)))
}

fn style_obj(ctx: &mut Context, i: usize) -> JsObject {
    let o = JsObject::with_object_proto(ctx.intrinsics());
    o.insert_property(js_string!("__ferris_node"), ro(JsValue::from(i as f64)));
    o.insert_property(
        js_string!("setProperty"),
        method(ctx, "setProperty", 2, style_set_property),
    );
    o.insert_property(
        js_string!("getPropertyValue"),
        method(ctx, "getPropertyValue", 1, style_get_property),
    );
    o
}

// ---------------------------------------------------------------------------
// document
// ---------------------------------------------------------------------------

fn doc_get_element_by_id(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_str(args, ctx, 0)?;
    let hit = with_doc(|d| d.by_id(&id));
    Ok(match hit {
        Some(h) => JsValue::from(element_obj(ctx, h)),
        None => JsValue::null(),
    })
}

fn doc_get_elements_by_tag(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let tag = arg_str(args, ctx, 0)?;
    let hits = with_doc(|d| d.by_tag(&tag));
    Ok(JsValue::from(nodes_to_array(ctx, hits)))
}

fn doc_get_elements_by_class(
    _t: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let class = arg_str(args, ctx, 0)?;
    let hits = with_doc(|d| d.by_class(&class));
    Ok(JsValue::from(nodes_to_array(ctx, hits)))
}

fn doc_query_selector(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let sel = arg_str(args, ctx, 0)?;
    let hit = with_doc(|d| d.match_simple(0, &sel).first().copied());
    Ok(match hit {
        Some(h) => JsValue::from(element_obj(ctx, h)),
        None => JsValue::null(),
    })
}

fn doc_query_selector_all(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let sel = arg_str(args, ctx, 0)?;
    let hits = with_doc(|d| d.match_simple(0, &sel));
    Ok(JsValue::from(nodes_to_array(ctx, hits)))
}

fn doc_create_element(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let tag = arg_str(args, ctx, 0)?;
    let i = with_doc(|d| d.new_element(&tag));
    Ok(JsValue::from(element_obj(ctx, i)))
}

fn doc_create_text_node(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = arg_str(args, ctx, 0)?;
    let i = with_doc(|d| d.new_text(&s));
    let o = JsObject::with_object_proto(ctx.intrinsics());
    o.insert_property(js_string!("__ferris_node"), ro(JsValue::from(i as f64)));
    o.insert_property(js_string!("nodeType"), ro(JsValue::from(3)));
    o.insert_property(
        js_string!("textContent"),
        accessor(ctx, el_text_content_get, Some(el_text_content_set)),
    );
    Ok(JsValue::from(o))
}

fn doc_write(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    // No live parser to write into: append to the body's markup instead,
    // which is what pre-DOMContentLoaded document.write effectively does.
    let html = arg_str(args, ctx, 0)?;
    with_doc(|d| {
        let body = d.body_element();
        let cur = d.inner_html(body);
        d.set_inner_html(body, &format!("{cur}{html}"));
    });
    Ok(JsValue::undefined())
}

fn doc_body_get(_t: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = with_doc(|d| d.body_element());
    Ok(JsValue::from(element_obj(ctx, i)))
}

fn doc_head_get(_t: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = with_doc(|d| d.head_element());
    Ok(JsValue::from(element_obj(ctx, i)))
}

fn doc_document_element(_t: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let i = with_doc(|d| d.html_element());
    Ok(JsValue::from(element_obj(ctx, i)))
}

fn doc_title_get(_t: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let title = with_doc(|d| {
        d.by_tag("title")
            .first()
            .map(|i| d.text_content(*i))
            .unwrap_or_default()
    });
    Ok(JsValue::from(js_string!(&*title)))
}

fn doc_title_set(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let v = arg_str(args, ctx, 0)?;
    with_doc(|d| {
        let head = d.head_element();
        let t = match d.by_tag("title").first().copied() {
            Some(t) => t,
            None => {
                let t = d.new_element("title");
                d.append(head, t);
                t
            }
        };
        d.set_text_content(t, &v);
    });
    Ok(JsValue::undefined())
}

fn doc_url_get(_t: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let u = WORLD.with(|w| {
        w.borrow()
            .as_ref()
            .map(|w| w.borrow().base.to_string())
            .unwrap_or_default()
    });
    Ok(JsValue::from(js_string!(&*u)))
}

fn doc_add_event_listener(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn document_obj(ctx: &mut Context) -> JsObject {
    let d = JsObject::with_object_proto(ctx.intrinsics());
    d.insert_property(
        js_string!("getElementById"),
        method(ctx, "getElementById", 1, doc_get_element_by_id),
    );
    d.insert_property(
        js_string!("getElementsByTagName"),
        method(ctx, "getElementsByTagName", 1, doc_get_elements_by_tag),
    );
    d.insert_property(
        js_string!("getElementsByClassName"),
        method(ctx, "getElementsByClassName", 1, doc_get_elements_by_class),
    );
    d.insert_property(
        js_string!("querySelector"),
        method(ctx, "querySelector", 1, doc_query_selector),
    );
    d.insert_property(
        js_string!("querySelectorAll"),
        method(ctx, "querySelectorAll", 1, doc_query_selector_all),
    );
    d.insert_property(
        js_string!("createElement"),
        method(ctx, "createElement", 1, doc_create_element),
    );
    d.insert_property(
        js_string!("createTextNode"),
        method(ctx, "createTextNode", 1, doc_create_text_node),
    );
    d.insert_property(js_string!("write"), method(ctx, "write", 1, doc_write));
    d.insert_property(js_string!("writeln"), method(ctx, "writeln", 1, doc_write));
    d.insert_property(
        js_string!("addEventListener"),
        method(ctx, "addEventListener", 2, doc_add_event_listener),
    );
    d.insert_property(js_string!("body"), accessor(ctx, doc_body_get, None));
    d.insert_property(js_string!("head"), accessor(ctx, doc_head_get, None));
    d.insert_property(
        js_string!("documentElement"),
        accessor(ctx, doc_document_element, None),
    );
    d.insert_property(
        js_string!("title"),
        accessor(ctx, doc_title_get, Some(doc_title_set)),
    );
    d.insert_property(js_string!("URL"), accessor(ctx, doc_url_get, None));
    d.insert_property(js_string!("nodeType"), ro(JsValue::from(9)));
    d
}

// ---------------------------------------------------------------------------
// console / timers / window bits
// ---------------------------------------------------------------------------

fn console_line(args: &[JsValue], ctx: &mut Context) -> String {
    let mut parts = vec![];
    for a in args {
        let s = match a.as_string() {
            Some(s) => s.to_std_string_escaped(),
            None => match a.to_string(ctx) {
                Ok(s) => s.to_std_string_escaped(),
                Err(_) => format!("{a:?}"),
            },
        };
        parts.push(s);
    }
    parts.join(" ")
}

fn push_console(level: &str, args: &[JsValue], ctx: &mut Context) {
    let line = format!("[{level}] {}", console_line(args, ctx));
    let w = world();
    let mut w = w.borrow_mut();
    if w.console.len() < MAX_CONSOLE_LINES {
        w.console.push(line);
    }
}

fn con_log(t: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    push_console("log", a, c);
    let _ = t;
    Ok(JsValue::undefined())
}
fn con_warn(t: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    push_console("warn", a, c);
    let _ = t;
    Ok(JsValue::undefined())
}
fn con_error(t: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    push_console("error", a, c);
    let _ = t;
    Ok(JsValue::undefined())
}

fn console_obj(ctx: &mut Context) -> JsObject {
    let c = JsObject::with_object_proto(ctx.intrinsics());
    c.insert_property(js_string!("log"), method(ctx, "log", 0, con_log));
    c.insert_property(js_string!("info"), method(ctx, "info", 0, con_log));
    c.insert_property(js_string!("debug"), method(ctx, "debug", 0, con_log));
    c.insert_property(js_string!("warn"), method(ctx, "warn", 0, con_warn));
    c.insert_property(js_string!("error"), method(ctx, "error", 0, con_error));
    c
}

/// Ferris has no event loop between paints: timeouts fire immediately, once.
fn win_set_timeout(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let cb = args.get_or_undefined(0).clone();
    if let Some(o) = cb.as_object()
        && o.is_callable()
    {
        let f = JsFunction::from_object(o).ok_or_else(|| throw("setTimeout: not callable"))?;
        f.typed::<(JsValue,), JsValue>()
            .call(ctx, (JsValue::undefined(),))?;
    }
    Ok(JsValue::from(1))
}

fn win_noop_id(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(0))
}
fn win_noop(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn win_raf(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let cb = args.get_or_undefined(0).clone();
    if let Some(o) = cb.as_object()
        && o.is_callable()
        && let Some(f) = JsFunction::from_object(o)
    {
        f.typed::<(f64,), JsValue>().call(ctx, (0.0,))?;
    }
    Ok(JsValue::from(1))
}

fn location_obj(ctx: &mut Context) -> JsObject {
    let (href, proto, host, path, search, hash) = WORLD.with(|w| {
        w.borrow()
            .as_ref()
            .map(|w| {
                let b = &w.borrow().base;
                (
                    b.to_string(),
                    b.scheme().to_string(),
                    b.host_str().unwrap_or("").to_string(),
                    b.path().to_string(),
                    b.query().map(|q| format!("?{q}")).unwrap_or_default(),
                    b.fragment().map(|f| format!("#{f}")).unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    });
    let o = JsObject::with_object_proto(ctx.intrinsics());
    o.insert_property(js_string!("href"), ro(JsValue::from(js_string!(&*href))));
    o.insert_property(
        js_string!("protocol"),
        ro(JsValue::from(js_string!(&*format!("{proto}:")))),
    );
    o.insert_property(
        js_string!("hostname"),
        ro(JsValue::from(js_string!(&*host))),
    );
    o.insert_property(js_string!("host"), ro(JsValue::from(js_string!(&*host))));
    o.insert_property(
        js_string!("pathname"),
        ro(JsValue::from(js_string!(&*path))),
    );
    o.insert_property(
        js_string!("search"),
        ro(JsValue::from(js_string!(&*search))),
    );
    o.insert_property(js_string!("hash"), ro(JsValue::from(js_string!(&*hash))));
    o
}

fn navigator_obj(ctx: &mut Context) -> JsObject {
    let o = JsObject::with_object_proto(ctx.intrinsics());
    o.insert_property(
        js_string!("userAgent"),
        ro(JsValue::from(js_string!(crate::net::USER_AGENT))),
    );
    o.insert_property(
        js_string!("language"),
        ro(JsValue::from(js_string!("en-US"))),
    );
    o.insert_property(
        js_string!("platform"),
        ro(JsValue::from(js_string!("Linux x86_64"))),
    );
    o
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

struct ScriptJob {
    code: String,
    origin: String,
}

fn collect_scripts(world: &mut World) -> Vec<ScriptJob> {
    let ids = world.doc.by_tag("script");
    let mut jobs = vec![];
    let mut total = 0usize;
    for i in ids {
        let ty = world
            .doc
            .attr(i, "type")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let js_ty = ty.is_empty()
            || ty == "text/javascript"
            || ty == "application/javascript"
            || ty == "javascript";
        if !js_ty || world.doc.attr(i, "nomodule").is_some() {
            continue;
        }
        if world.doc.attr(i, "type") == Some("module") {
            continue;
        }
        let origin;
        let code = match world.doc.attr(i, "src").map(str::to_string) {
            Some(src) => {
                let abs = match world.base.join(&src) {
                    Ok(u) => u,
                    Err(e) => {
                        world.errors.push(format!("bad script src {src}: {e}"));
                        continue;
                    }
                };
                origin = abs.to_string();
                match crate::net::fetch_cached(&abs, SCRIPT_FETCH_BYTES, Duration::from_secs(60)) {
                    Ok(f) => f.text(),
                    Err(e) => {
                        world
                            .errors
                            .push(format!("fetch {abs} failed: {}", e.headline()));
                        continue;
                    }
                }
            }
            None => {
                origin = world.base.to_string();
                world.doc.text_content(i)
            }
        };
        total += code.len();
        if total > MAX_SCRIPT_BYTES {
            world
                .errors
                .push("script budget exceeded; remaining scripts skipped".to_string());
            break;
        }
        jobs.push(ScriptJob { code, origin });
    }
    jobs
}

fn err_string(e: &JsError, _ctx: &mut Context) -> String {
    format!("{e}")
}

/// Run the page's scripts against a mirror of `html`; returns the mutated
/// HTML plus a report. Never panics on script errors.
pub fn preprocess(html: &str, base: &str) -> (String, JsReport) {
    let started = Instant::now();
    let base_url = Url::parse(base).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
    let mut w = World {
        doc: Doc::from_html(html),
        base: base_url,
        console: vec![],
        errors: vec![],
        scripts: 0,
    };
    let jobs = collect_scripts(&mut w);
    let mut report = JsReport {
        enabled: true,
        ..Default::default()
    };
    if jobs.is_empty() {
        report.ms = started.elapsed().as_millis() as u64;
        return (html.to_string(), report);
    }

    let world = Rc::new(RefCell::new(w));
    WORLD.with(|slot| *slot.borrow_mut() = Some(world.clone()));

    let result = run_scripts(jobs);

    WORLD.with(|slot| *slot.borrow_mut() = None);

    let w = world.borrow();
    if let Err(e) = result {
        report.errors.push(format!("context: {e}"));
    }
    report.scripts = w.scripts;
    report.console = w.console.clone();
    report
        .errors
        .extend(w.errors.iter().take(MAX_ERROR_LINES).cloned());
    report.ms = started.elapsed().as_millis() as u64;
    let out = w.doc.to_html();
    (out, report)
}

/// Create one Boa context, install the DOM globals and evaluate every script
/// job in order. Script errors are captured into the world, never propagated
/// as panics.
fn run_scripts(jobs: Vec<ScriptJob>) -> JsResult<()> {
    let mut ctx = Context::default();
    ctx.runtime_limits_mut()
        .set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
    ctx.runtime_limits_mut()
        .set_recursion_limit(RECURSION_LIMIT);

    let g = ctx.global_object();
    g.insert_property(
        js_string!("document"),
        data(JsValue::from(document_obj(&mut ctx))),
    );
    g.insert_property(
        js_string!("console"),
        data(JsValue::from(console_obj(&mut ctx))),
    );
    g.insert_property(
        js_string!("location"),
        data(JsValue::from(location_obj(&mut ctx))),
    );
    g.insert_property(
        js_string!("navigator"),
        data(JsValue::from(navigator_obj(&mut ctx))),
    );
    g.insert_property(
        js_string!("window"),
        data(JsValue::from(ctx.global_object())),
    );
    g.insert_property(
        js_string!("setTimeout"),
        method(&mut ctx, "setTimeout", 2, win_set_timeout),
    );
    g.insert_property(
        js_string!("setInterval"),
        method(&mut ctx, "setInterval", 2, win_noop_id),
    );
    g.insert_property(
        js_string!("clearInterval"),
        method(&mut ctx, "clearInterval", 1, win_noop),
    );
    g.insert_property(
        js_string!("clearTimeout"),
        method(&mut ctx, "clearTimeout", 1, win_noop),
    );
    g.insert_property(
        js_string!("requestAnimationFrame"),
        method(&mut ctx, "requestAnimationFrame", 1, win_raf),
    );
    g.insert_property(js_string!("alert"), method(&mut ctx, "alert", 1, win_noop));
    g.insert_property(js_string!("focus"), method(&mut ctx, "focus", 0, win_noop));

    // IMPORTANT: no RefCell borrow may be held across `ctx.eval` — the
    // scripts call back into our natives, which borrow the world themselves.
    for job in jobs {
        world().borrow_mut().scripts += 1;
        let evaluated = ctx.eval(Source::from_bytes(job.code.as_bytes()));
        if let Err(e) = evaluated {
            let msg = err_string(&e, &mut ctx);
            let guard = world();
            let mut w = guard.borrow_mut();
            if w.errors.len() < MAX_ERROR_LINES {
                w.errors.push(format!(
                    "{}: {}",
                    job.origin,
                    msg.lines().next().unwrap_or("")
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(html: &str) -> (String, JsReport) {
        preprocess(html, "https://test.ferris/")
    }

    #[test]
    fn script_mutates_dom_before_render() {
        let (html, r) = run(r#"<html><body><div id="out">old</div>
               <script>document.getElementById('out').textContent = 'new';</script>
               </body></html>"#);
        assert_eq!(r.scripts, 1);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains(r#"<div id="out">new</div>"#));
    }

    #[test]
    fn create_append_and_class() {
        let (html, r) = run(r#"<html><body><ul id="l"></ul><script>
               var li = document.createElement('li');
               li.className = 'item';
               li.textContent = 'one';
               document.getElementById('l').appendChild(li);
               </script></body></html>"#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains(r#"<li class="item">one</li>"#));
    }

    #[test]
    fn inner_html_and_query() {
        let (html, r) = run(r#"<html><body><div id="a"></div><script>
               document.getElementById('a').innerHTML = '<p class="x">hi</p><p class="x">yo</p>';
               var n = document.querySelectorAll('.x').length;
               document.title = 'count ' + n;
               </script></body></html>"#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains("<p class=\"x\">hi</p>"));
        assert!(html.contains("<title>count 2</title>"));
    }

    #[test]
    fn console_is_captured() {
        let (_, r) = run(
            r#"<html><body><script>console.log('hello', 42); console.warn('careful');</script></body></html>"#,
        );
        assert_eq!(r.console.len(), 2);
        assert!(r.console[0].contains("hello 42"));
        assert!(r.console[1].contains("careful"));
    }

    #[test]
    fn runaway_loop_is_contained() {
        let (html, r) = run(
            r#"<html><body><p>before</p><script>while (true) {}</script><p>after</p></body></html>"#,
        );
        assert_eq!(r.errors.len(), 1, "loop limit must surface as one error");
        assert!(html.contains("<p>before</p>"), "page still renders");
    }

    #[test]
    fn script_error_does_not_kill_the_page() {
        let (html, r) =
            run(r#"<html><body><p>ok</p><script>nope.not_a_thing();</script></body></html>"#);
        assert_eq!(r.scripts, 1);
        assert_eq!(r.errors.len(), 1);
        assert!(html.contains("<p>ok</p>"));
    }

    #[test]
    fn style_set_property_reaches_the_cascade() {
        let (html, r) = run(r#"<html><body><div id="d">x</div><script>
               document.getElementById('d').style.setProperty('color', 'rgb(255, 0, 0)');
               </script></body></html>"#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains("style=\"color: rgb(255, 0, 0)\""));
    }

    #[test]
    fn two_scripts_share_state_in_order() {
        let (html, r) = run(r#"<html><body><div id="o"></div>
               <script>window.val = 7;</script>
               <script>document.getElementById('o').textContent = window.val * 6;</script>
               </body></html>"#);
        assert_eq!(r.scripts, 2);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains(r#"<div id="o">42</div>"#));
    }

    #[test]
    fn class_list_and_attributes() {
        let (html, r) = run(r#"<html><body><div id="d" class="a">x</div><script>
               var d = document.getElementById('d');
               d.classList.add('b'); d.classList.toggle('a');
               d.setAttribute('data-x', '1');
               if (!d.hasAttribute('data-x')) throw new Error('attr');
               </script></body></html>"#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains(r#"class="b""#));
        assert!(html.contains(r#"data-x="1""#));
    }

    #[test]
    fn document_write_appends_to_body() {
        let (html, r) = run(
            r#"<html><body><p>start</p><script>document.write('<p>written</p>');</script></body></html>"#,
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(html.contains("<p>written</p>"));
    }
}

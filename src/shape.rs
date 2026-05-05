use indexmap::IndexMap;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Array(Box<Shape>),
    Object(IndexMap<Arc<str>, Shape>),
    Union(Vec<Shape>),
    Unknown,
}

impl Shape {
    pub fn of(v: &Value) -> Shape {
        match v {
            Value::Null      => Shape::Null,
            Value::Bool(_)   => Shape::Bool,
            Value::Number(n) => if n.is_f64() { Shape::Float } else { Shape::Int },
            Value::String(_) => Shape::Str,
            Value::Array(a)  => {
                let mut elem = Shape::Unknown;
                let mut first = true;
                for item in a {
                    let s = Shape::of(item);
                    elem = if first { first = false; s } else { elem.merge(s) };
                }
                Shape::Array(Box::new(elem))
            }
            Value::Object(o) => {
                let mut map = IndexMap::new();
                for (k, v) in o {
                    map.insert(Arc::from(k.as_str()), Shape::of(v));
                }
                Shape::Object(map)
            }
        }
    }

    pub fn merge(self, other: Shape) -> Shape {
        match (self, other) {
            (a, b) if a == b => a,
            (Shape::Unknown, x) | (x, Shape::Unknown) => x,
            (Shape::Array(a), Shape::Array(b)) => Shape::Array(Box::new(a.merge(*b))),
            (Shape::Object(mut a), Shape::Object(b)) => {
                for (k, v) in b {
                    if let Some(existing) = a.shift_remove(&k) {
                        a.insert(k, existing.merge(v));
                    } else {
                        a.insert(k, v);
                    }
                }
                Shape::Object(a)
            }
            (Shape::Int, Shape::Float) | (Shape::Float, Shape::Int) => Shape::Float,
            (Shape::Union(mut xs), y) | (y, Shape::Union(mut xs)) => {
                if !xs.contains(&y) { xs.push(y); }
                Shape::Union(xs)
            }
            (a, b) => Shape::Union(vec![a, b]),
        }
    }

    pub fn field(&self, name: &str) -> Option<&Shape> {
        match self {
            Shape::Object(m) => m.get(name),
            _ => None,
        }
    }

    pub fn element(&self) -> Option<&Shape> {
        match self {
            Shape::Array(b) => Some(b),
            _ => None,
        }
    }
}

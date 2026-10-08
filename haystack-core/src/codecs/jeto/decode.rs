use super::*;
use super::{
    context::Class,
    parser::{Node, Object},
    storage::Columns,
};
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::HRef;
#[derive(Default)]
struct DictShape<'a> {
    columns: Option<&'a Columns<'a>>,
    extra: usize,
}
pub(super) fn value<M: Meter>(
    wire: &Node<'_>,
    context: &Context,
    expected: Option<&str>,
    meter: &mut M,
    depth: usize,
) -> Result<Kind, Error<M::Error>> {
    charge(meter, Charge::Depth(depth))?;
    charge(meter, Charge::Nodes(1))?;
    charge(meter, Charge::Work(1))?;
    let class = expected
        .map(|name| resolve(context, name, meter))
        .transpose()?;
    Ok(match wire {
        Node::Null => Kind::Null,
        Node::Bool(b) => Kind::Bool(*b),
        Node::Number(text) => {
            charge(meter, Charge::Work(text.len().saturating_mul(4)))?;
            scalar::numeric(text, class)?
        }
        Node::Str(text) => {
            text_charge(text, context, meter)?;
            scalar::string(text, context, expected, class)?
        }
        Node::List(items) => {
            let of = match class {
                Some(Class::List(of)) => of.as_deref(),
                _ => None,
            };
            charge(
                meter,
                Charge::Retained(
                    items
                        .len()
                        .saturating_mul(std::mem::size_of::<Kind>())
                        .saturating_add(32),
                ),
            )?;
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(value(item, context, of, meter, depth + 1)?);
            }
            Kind::List(values)
        }
        Node::Object(members) => {
            let spec = match members.get("spec") {
                Some(Node::Str(spec)) => Some(spec.as_ref()),
                None => None,
                _ => return Err(invalid("structural spec must be a string")),
            };
            let own = spec.map(|name| resolve(context, name, meter)).transpose()?;
            if own.is_some_and(Class::scalar) {
                let Node::Str(text) = members
                    .get("val")
                    .ok_or_else(|| invalid("boxed scalar requires val"))?
                else {
                    return Err(invalid("boxed scalar val must be string"));
                };
                let is_ref = matches!(own, Some(Class::Scalar(super::context::Scalar::Ref)));
                if members
                    .keys()
                    .any(|k| k != "spec" && k != "val" && !(is_ref && k == "dis"))
                {
                    return Err(invalid("unknown boxed scalar field"));
                }
                text_charge(text, context, meter)?;
                let mut value = scalar::string(text, context, spec, own)?;
                if let Some(display) = members.get("dis") {
                    let (Kind::Ref(reference), Node::Str(display)) = (&mut value, display) else {
                        return Err(invalid("Ref display must be string"));
                    };
                    charge(meter, Charge::Work(display.len()))?;
                    charge(meter, Charge::Retained(display.len().saturating_add(32)))?;
                    reference.dis = Some(display.to_string());
                }
                return Ok(value);
            }
            match own.or(class) {
                Some(Class::Grid(of)) => grid(members, context, spec, of.as_deref(), meter, depth)?,
                Some(Class::List(_)) if spec.is_some() => {
                    return Err(invalid("List uses JSON array syntax"));
                }
                Some(c) if spec.is_some() && !matches!(c, Class::Dict(_)) => {
                    return Err(invalid("spec does not identify a Dict"));
                }
                effective => Kind::Dict(Box::new(dict(
                    members,
                    context,
                    spec,
                    effective,
                    DictShape::default(),
                    meter,
                    depth,
                )?)),
            }
        }
    })
}
fn dict<M: Meter>(
    members: &Object<'_>,
    context: &Context,
    spec: Option<&str>,
    class: Option<&Class>,
    shape: DictShape<'_>,
    meter: &mut M,
    depth: usize,
) -> Result<HDict, Error<M::Error>> {
    let capacity = members
        .len()
        .checked_add(shape.extra)
        .ok_or(Error::Allocation)?;
    let mut dict = storage::dict(capacity, meter)?;
    for (name, member) in members {
        charge(meter, Charge::Work(name.len().saturating_add(1)))?;
        if name == "spec" {
            insert_ref(
                &mut dict,
                "spec",
                spec.ok_or_else(|| invalid("spec must be a string"))?,
                meter,
            )?;
            continue;
        }
        let column_type = if let Some(columns) = shape.columns {
            tree_charge(name, columns.len(), meter)?;
            Some(
                columns
                    .get(name.as_ref())
                    .ok_or_else(|| invalid("row key has no column"))?,
            )
        } else {
            None
        };
        if matches!(member, Node::Null) {
            continue;
        }
        let expected = column_type
            .copied()
            .flatten()
            .or(member_type(class, name, meter)?);
        charge(meter, Charge::Retained(name.len()))?;
        dict.set(
            name.as_ref(),
            value(member, context, expected, meter, depth + 1)?,
        );
    }
    Ok(dict)
}
fn insert_ref<M: Meter>(
    dict: &mut HDict,
    key: &str,
    name: &str,
    meter: &mut M,
) -> Result<(), Error<M::Error>> {
    charge(meter, Charge::Nodes(1))?;
    charge(meter, Charge::Work(name.len().saturating_add(key.len())))?;
    charge(
        meter,
        Charge::Retained(name.len().saturating_add(key.len())),
    )?;
    dict.set(key, Kind::Ref(HRef::new(name, None)));
    Ok(())
}
fn structural<'a, M: Meter>(
    members: &'a Object<'_>,
    key: &str,
    meter: &mut M,
) -> Result<Option<&'a str>, Error<M::Error>> {
    tree_charge(key, members.len(), meter)?;
    match members.get(key) {
        None => Ok(None),
        Some(Node::Str(s)) => Ok(Some(s)),
        _ => Err(invalid("structural property must be a string")),
    }
}
fn metadata<M: Meter>(
    wire: Option<&Node<'_>>,
    context: &Context,
    meter: &mut M,
    depth: usize,
    extra: usize,
) -> Result<HDict, Error<M::Error>> {
    let Some(wire) = wire else {
        return storage::dict(extra, meter);
    };
    let Node::Object(members) = wire else {
        return Err(invalid("metadata must be a Dict"));
    };
    if members.contains_key("spec") || members.contains_key("of") {
        return Err(invalid("reserved metadata collision"));
    }
    charge(meter, Charge::Depth(depth))?;
    charge(meter, Charge::Nodes(1))?;
    dict(
        members,
        context,
        None,
        None,
        DictShape {
            columns: None,
            extra,
        },
        meter,
        depth,
    )
}
fn grid<M: Meter>(
    members: &Object<'_>,
    context: &Context,
    spec: Option<&str>,
    inherited_of: Option<&str>,
    meter: &mut M,
    depth: usize,
) -> Result<Kind, Error<M::Error>> {
    for name in members.keys() {
        charge(meter, Charge::Work(name.len().saturating_add(1)))?;
        if !matches!(name.as_ref(), "spec" | "of" | "meta" | "cols" | "rows") {
            return Err(invalid("unknown grid field"));
        }
    }
    let of = structural(members, "of", meter)?;
    if of.is_some() && inherited_of.is_some() {
        return Err(invalid("grid of conflicts with declared row type"));
    }
    let row_type = of.or(inherited_of);
    let row_class = row_type
        .map(|name| resolve(context, name, meter))
        .transpose()?;
    if row_class.is_some_and(|class| !matches!(class, Class::Dict(_))) {
        return Err(invalid("grid of must be a Dict"));
    }
    let mut meta = metadata(
        members.get("meta"),
        context,
        meter,
        depth + 1,
        usize::from(spec.is_some_and(|s| s != "sys::Grid")) + usize::from(of.is_some()),
    )?;
    if let Some(spec) = spec.filter(|s| *s != "sys::Grid") {
        insert_ref(&mut meta, "spec", spec, meter)?;
    }
    if let Some(of) = of {
        insert_ref(&mut meta, "of", of, meter)?;
    }
    let Some(Node::List(cols)) = members.get("cols") else {
        return Err(invalid("grid requires cols array"));
    };
    let Some(Node::List(rows)) = members.get("rows") else {
        return Err(invalid("grid requires rows array"));
    };
    charge(
        meter,
        Charge::Retained(
            cols.len()
                .saturating_mul(std::mem::size_of::<HCol>())
                .saturating_add(rows.len().saturating_mul(std::mem::size_of::<HDict>()))
                .saturating_add(128),
        ),
    )?;
    let mut columns = Vec::with_capacity(cols.len());
    // The local map borrows parser storage and is built once; sparse rows never
    // clone all column contexts or synthesize absent cells.
    let mut column_types = Columns::with_capacity(cols.len(), meter)?;
    for col in cols {
        charge(meter, Charge::Nodes(1))?;
        charge(meter, Charge::Depth(depth + 2))?;
        let Node::Object(col) = col else {
            return Err(invalid("column must be an object"));
        };
        for key in col.keys() {
            charge(meter, Charge::Work(key.len().saturating_add(1)))?;
            if !matches!(key.as_ref(), "name" | "of" | "meta") {
                return Err(invalid("unknown column field"));
            }
        }
        let name =
            structural(col, "name", meter)?.ok_or_else(|| invalid("column requires name"))?;
        if name.is_empty() || name == "spec" {
            return Err(invalid("invalid or reserved column name"));
        }
        let of = structural(col, "of", meter)?;
        if let Some(of) = of {
            resolve(context, of, meter)?;
        }
        column_types.push(name, of, meter)?;
        let mut meta = metadata(
            col.get("meta"),
            context,
            meter,
            depth + 3,
            usize::from(of.is_some()),
        )?;
        if let Some(of) = of {
            insert_ref(&mut meta, "of", of, meter)?;
        }
        charge(meter, Charge::Work(name.len()))?;
        charge(meter, Charge::Retained(name.len()))?;
        columns.push(HCol::with_meta(name, meta));
    }
    if !column_types.finish(meter)? {
        return Err(invalid("duplicate column name"));
    }
    let mut decoded_rows = Vec::with_capacity(rows.len());
    for row in rows {
        charge(meter, Charge::Depth(depth + 2))?;
        charge(meter, Charge::Nodes(1))?;
        let Node::Object(row) = row else {
            return Err(invalid("grid row must be a Dict"));
        };
        let spec = structural(row, "spec", meter)?;
        let class = spec
            .map(|name| resolve(context, name, meter))
            .transpose()?
            .or(row_class);
        if class.is_some_and(|class| !matches!(class, Class::Dict(_))) {
            return Err(invalid("row spec must be a Dict"));
        }
        decoded_rows.push(dict(
            row,
            context,
            spec,
            class,
            DictShape {
                columns: Some(&column_types),
                extra: 0,
            },
            meter,
            depth + 2,
        )?);
    }
    Ok(Kind::Grid(Box::new(HGrid::from_parts(
        meta,
        columns,
        decoded_rows,
    ))))
}

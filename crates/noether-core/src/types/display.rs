use crate::types::NType;
use std::fmt;

impl fmt::Display for NType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NType::Text => write!(f, "Text"),
            NType::Number => write!(f, "Number"),
            NType::Bool => write!(f, "Bool"),
            NType::Bytes => write!(f, "Bytes"),
            NType::Null => write!(f, "Null"),
            NType::Any => write!(f, "Any"),
            NType::List(inner) => write!(f, "List<{inner}>"),
            NType::Stream(inner) => write!(f, "Stream<{inner}>"),
            NType::Map { key, value } => write!(f, "Map<{key}, {value}>"),
            NType::Record(fields) => {
                write!(f, "Record {{ ")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: {ty}")?;
                }
                write!(f, " }}")
            }
            NType::Union(variants) => {
                for (i, v) in variants.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{v}")?;
                }
                Ok(())
            }
            NType::VNode => write!(f, "VNode"),
            // Angle brackets mark it visibly as a placeholder variable rather
            // than a concrete type name — matches the informal `<T>` / `<U>`
            // notation used in docs/roadmap and the unification tests.
            NType::Var(name) => write!(f, "<{name}>"),
            // Row-polymorphic record: known fields first, then a `...R`
            // tail marking the captured rest. Reads like the informal
            // record-extension notation from the ML family.
            NType::RecordWith { fields, rest } => {
                write!(f, "Record {{ ")?;
                for (name, ty) in fields.iter() {
                    write!(f, "{name}: {ty}, ")?;
                }
                write!(f, "...<{rest}> }}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::NType;

    #[test]
    fn display_primitives() {
        assert_eq!(format!("{}", NType::Text), "Text");
        assert_eq!(format!("{}", NType::Number), "Number");
        assert_eq!(format!("{}", NType::Any), "Any");
    }

    #[test]
    fn display_list() {
        assert_eq!(
            format!("{}", NType::List(Box::new(NType::Text))),
            "List<Text>"
        );
    }

    #[test]
    fn display_record() {
        let r = NType::record([("name", NType::Text), ("age", NType::Number)]);
        assert_eq!(format!("{r}"), "Record { age: Number, name: Text }");
    }

    #[test]
    fn display_union() {
        let u = NType::union(vec![NType::Text, NType::Null]);
        assert_eq!(format!("{u}"), "Null | Text");
    }

    #[test]
    fn display_nested() {
        let t = NType::List(Box::new(NType::record([("x", NType::Number)])));
        assert_eq!(format!("{t}"), "List<Record { x: Number }>");
    }

    #[test]
    fn display_vnode() {
        assert_eq!(format!("{}", NType::VNode), "VNode");
    }

    #[test]
    fn display_var() {
        assert_eq!(format!("{}", NType::Var("T".into())), "<T>");
        assert_eq!(format!("{}", NType::Var("Element".into())), "<Element>");
    }

    #[test]
    fn display_list_of_var() {
        let t = NType::List(Box::new(NType::Var("T".into())));
        assert_eq!(format!("{t}"), "List<<T>>");
    }
}

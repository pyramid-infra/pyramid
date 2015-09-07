
extern crate xml;
peg_file! pon_parse("pon.rustpeg");

use pon::*;

use std::fs::File;
use std::io::BufReader;
use std::collections::HashMap;
use std::collections::hash_map::Keys;
use std::collections::hash_map::Entry;
use std::path::Path;
use std::fs::PathExt;
use std::io::Write;
use std::cell::RefCell;
use std::rc::Rc;

use xml::reader::EventReader;
use xml::reader::events::*;

#[derive(PartialEq, Debug, Clone)]
pub enum DocError {
    PonTranslateErr(PonTranslateErr),
    BadReference,
    NoSuchProperty(String),
    NoSuchEntity,
    InvalidParent
}

impl From<PonTranslateErr> for DocError {
    fn from(err: PonTranslateErr) -> DocError {
        DocError::PonTranslateErr(err)
    }
}

pub type EntityId = u64;

pub type EntityIter<'a> = Keys<'a, EntityId, Entity>;
pub type PropertyIter<'a> = Keys<'a, String, Property>;


#[derive(Debug)]
struct Property {
    expression: Rc<RefCell<Pon>>,
    dependants: Vec<PropRef>
}

#[derive(Debug)]
struct Entity {
    id: EntityId,
    type_name: String,
    properties: HashMap<String, Property>,
    name: Option<String>,
    children_ids: Vec<EntityId>,
    parent_id: EntityId
}

impl Entity {
    fn get_or_create_property(&mut self, key: &str) -> &Property {
        match self.properties.entry(key.to_string()) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(Property {
                expression: Rc::new(RefCell::new(Pon::Nil)),
                dependants: vec![]
            })
        }
    }
}

pub struct Document {
    id_counter: EntityId,
    root: EntityId,
    entities: HashMap<EntityId, Entity>,
    entity_ids_by_name: HashMap<String, EntityId>
}

impl Document {
    pub fn new() -> Document {
        Document {
            id_counter: 0,
            root: 1,
            entities: HashMap::new(),
            entity_ids_by_name: HashMap::new()
        }
    }
    fn new_id(&mut self) -> EntityId {
        self.id_counter += 1;
        return self.id_counter;
    }
    pub fn append_entity(&mut self, parent_id: EntityId, type_name: String, name: Option<String>) -> Result<EntityId, DocError> {
        let id = self.new_id();
        let entity = Entity {
            id: id.clone(),
            type_name: type_name,
            properties: HashMap::new(),
            name: name,
            parent_id: parent_id,
            children_ids: vec![]
        };
        if parent_id != -1 {
            let parent = match self.entities.get_mut(&parent_id) {
                Some(parent) => parent,
                None => return Err(DocError::InvalidParent)
            };
            parent.children_ids.push(id);
        }
        if let &Some(ref name) = &entity.name {
            self.entity_ids_by_name.insert(name.clone(), entity.id);
        }
        self.entities.insert(entity.id, entity);
        return Ok(id);
    }
    pub fn get_entity_by_name(&self, name: &str) -> Option<EntityId> {
        match self.entity_ids_by_name.get(&name.to_string()) {
            Some(id) => Some(id.clone()),
            None => None
        }
    }
    pub fn iter(&self) -> EntityIter {
        self.entities.keys()
    }
    pub fn get_root(&self) -> &EntityId {
        &self.root
    }
    // returns all props that were invalidated
    pub fn set_property(&mut self, entity_id: &EntityId, name: &str, expression: Pon) -> Result<Vec<PropRef>, DocError> {
        //println!("set property {} {:?}", name, expression);
        let mut dependencies: Vec<PropRef> = {
            let entity = match self.entities.get(entity_id) {
                Some(entity) => entity,
                None => return Err(DocError::NoSuchEntity)
            };
            try!(self.build_property_node_dependencies(entity, &expression))
        };
        for PropRef { entity_id: dep_ent_id, property_key: dep_prop_key } in dependencies {
            match self.entities.get_mut(&dep_ent_id) {
                Some(dep_ent) => {
                    match dep_ent.properties.get_mut(&dep_prop_key) {
                        Some(property) => {
                            property.dependants.push(PropRef { entity_id: entity_id.clone(), property_key: name.to_string() });
                        },
                        None => return Err(DocError::BadReference)
                    }
                },
                None => return Err(DocError::BadReference)
            }
        }
        let resolved_expression = {
            Rc::new(RefCell::new(self.resolve_pon_dependencies(entity_id, &expression).unwrap()))
        };
        {
            let mut ent_mut = self.entities.get_mut(entity_id).unwrap();
            if ent_mut.properties.contains_key(&name.to_string()) {
                let mut prop = ent_mut.properties.get_mut(&name.to_string()).unwrap();
                prop.expression = resolved_expression;
            } else {
                ent_mut.properties.insert(name.to_string(), Property {
                    expression: resolved_expression,
                    dependants: vec![]
                });
            }
        }
        let entity = self.entities.get(entity_id).unwrap();
        let mut cascades = vec![PropRef { entity_id: entity_id.clone(), property_key: name.to_string() }];
        try!(self.build_property_cascades(entity, name.to_string(), &mut cascades));
        return Ok(cascades);
    }
    pub fn get_property_value(&self, entity_id: &EntityId, name: &str) -> Result<Pon, DocError> {
        match self.entities.get(entity_id) {
            Some(entity) => self.get_entity_property_value(entity, name.to_string()),
            None => Err(DocError::NoSuchEntity)
        }
    }
    pub fn has_property(&self, entity_id: &EntityId, name: &str) -> Result<bool, DocError> {
        match self.entities.get(entity_id) {
            Some(entity) => Ok(entity.properties.contains_key(name)),
            None => Err(DocError::NoSuchEntity)
        }
    }
    pub fn get_properties(&self, entity_id: &EntityId) -> Result<Vec<PropRef>, DocError> {
        match self.entities.get(&entity_id) {
            Some(entity) => Ok(entity.properties.keys().map(|key| PropRef { entity_id: entity_id.clone(), property_key: key.clone() }).collect()),
            None => Err(DocError::NoSuchEntity)
        }
    }
    pub fn get_children(&self, entity_id: &EntityId) -> Result<&Vec<EntityId>, DocError> {
        match self.entities.get(&entity_id) {
            Some(entity) => Ok(&entity.children_ids),
            None => Err(DocError::NoSuchEntity)
        }
    }
    pub fn search_children(&self, entity_id: &EntityId, name: &str) -> Result<EntityId, DocError> {
        match self.entities.get(entity_id) {
            Some(entity) => {
                if let &Some(ref string) = &entity.name {
                    if string == name {
                        return Ok(entity.id);
                    }
                }
                for c in &entity.children_ids {
                    match self.search_children(&c, name) {
                        Ok(id) => return Ok(id),
                        _ => {}
                    }
                }
                Err(DocError::BadReference)
            },
            None => Err(DocError::BadReference)
        }
    }
    pub fn resolve_entity_path(&self, start_entity_id: &EntityId, path: &EntityPath) -> Result<EntityId, DocError> {
        match path {
            &EntityPath::This => Ok(*start_entity_id),
            &EntityPath::Parent => match self.entities.get(start_entity_id) {
                Some(entity) => Ok(entity.parent_id.clone()),
                None => Err(DocError::BadReference)
            },
            &EntityPath::Named(ref name) => match self.entity_ids_by_name.get(name) {
                Some(entity_id) => Ok(entity_id.clone()),
                None => Err(DocError::BadReference)
            },
            &EntityPath::Search(ref path, ref search) => {
                match self.resolve_entity_path(start_entity_id, path) {
                    Ok(ent) => self.search_children(&ent, search),
                    Err(err) => Err(err)
                }
            }
        }
    }
    pub fn resolve_named_prop_ref(&self, start_entity_id: &EntityId, named_prop_ref: &NamedPropRef) -> Result<PropRef, DocError> {
        let owner_entity_id = try!(self.resolve_entity_path(start_entity_id, &named_prop_ref.entity_path));
        Ok(PropRef { entity_id: owner_entity_id, property_key: named_prop_ref.property_key.clone() })
    }
    pub fn get_entity_type_name(&self, entity_id: &EntityId) -> Result<&String, DocError> {
        match self.entities.get(&entity_id) {
            Some(entity) => Ok(&entity.type_name),
            None => Err(DocError::NoSuchEntity)
        }
    }

    pub fn from_file(path: &Path) -> Document {
        let root_path = path.parent().unwrap();
        let mut doc = Document::new();
        doc.append_from_event_reader(&root_path, &mut vec![], event_reader_from_file(path).events());
        return doc;
    }
    pub fn from_string(string: &str) -> Document {
        let mut doc = Document::new();
        let mut parser = EventReader::from_str(string);
        doc.append_from_event_reader(&Path::new("."), &mut vec![], parser.events());
        return doc;
    }


    fn build_property_node_dependencies(&self, entity: &Entity, node: &Pon) -> Result<Vec<PropRef>, DocError> {
        let mut named_refs = vec![];
        node.get_dependency_references(&mut named_refs);
        let mut refs = vec![];
        for named_prop_ref in named_refs {
            refs.push(try!(self.resolve_named_prop_ref(&entity.id, &named_prop_ref)));
        }
        return Ok(refs);
    }

    // get a list of properties that are invalid if property (entity, key) changes
    fn build_property_cascades(&self, entity: &Entity, key: String, cascades: &mut Vec<PropRef>) -> Result<(), DocError> {
        match entity.properties.get(&key) {
            Some(property) => {
                for pr in &property.dependants {
                    cascades.push(pr.clone());
                    try!(self.build_property_cascades(self.entities.get(&pr.entity_id).unwrap(), pr.property_key.clone(), cascades));
                }
                return Ok(());
            },
            None => Err(DocError::NoSuchProperty(key.to_string()))
        }
    }

    fn resolve_pon_dependencies(&mut self, entity_id: &EntityId, node: &Pon) -> Result<Pon, DocError> {
        match node {
            &Pon::TypedPon(box TypedPon { ref type_name, ref data }) =>
                Ok(Pon::TypedPon(Box::new(TypedPon {
                    type_name: type_name.clone(),
                    data: try!(self.resolve_pon_dependencies(entity_id, data))
                }))),
            &Pon::DependencyReference(ref named_prop_ref) => {
                let prop_ref = try!(self.resolve_named_prop_ref(&entity_id, &named_prop_ref));
                match self.entities.get_mut(&prop_ref.entity_id) {
                    Some(entity) => Ok(Pon::ResolvedDependencyReference(entity.get_or_create_property(&prop_ref.property_key).expression.clone())),
                    None => Err(DocError::BadReference)
                }
            },
            &Pon::Object(ref hm) => Ok(Pon::Object(hm.iter().map(|(k,v)| {
                    (k.clone(), self.resolve_pon_dependencies(entity_id, v).unwrap())
                }).collect())),
            &Pon::Array(ref arr) => Ok(Pon::Array(arr.iter().map(|v| {
                    self.resolve_pon_dependencies(entity_id, v).unwrap()
                }).collect())),
            _ => Ok(node.clone())
        }
    }

    fn get_entity_property_value(&self, entity: &Entity, name: String) -> Result<Pon, DocError> {
        match entity.properties.get(&name) {
            Some(prop) => Ok(prop.expression.borrow().clone()),
            None => Err(DocError::NoSuchProperty(name.to_string()))
        }
    }

    fn append_from_event_reader<T: Iterator<Item=XmlEvent>>(&mut self, root_path: &Path, mut entity_stack: &mut Vec<EntityId>, mut events: T) {
        while let Some(e) = events.next() {
            match e {
                XmlEvent::StartElement { name: type_name, attributes, .. } => {
                    let mut entity_name = match attributes.iter().find(|x| x.name.local_name == "name") {
                        Some(attr) => Some(attr.value.to_string()),
                        None => None
                    };
                    if type_name.local_name == "Include" {
                        let include_file = match attributes.iter().find(|x| x.name.local_name == "file" ) {
                            Some(file) => file.value.clone(),
                            None => panic!("Include file field missing")
                        };
                        let include_path_buf = root_path.join(include_file);
                        let include_path = include_path_buf.as_path();
                        if !include_path.exists() {
                            panic!("Include: No such file: {:?}", include_path);
                        }
                        let mut include_event_reader = event_reader_from_file(include_path);
                        let include_root_path = include_path.parent().unwrap();
                        self.append_from_event_reader(&include_root_path, &mut entity_stack, include_event_reader.events());
                        continue;
                    }
                    let parent = match entity_stack.last() {
                        Some(parent) => *parent,
                        None => -1
                    };
                    let entity_id = self.append_entity(parent, type_name.local_name.to_string(), entity_name).unwrap();

                    for attribute in attributes {
                        if (attribute.name.local_name == "name") { continue; }
                        match pon_parse::body(&attribute.value) {
                            Ok(node) => self.set_property(&entity_id, &attribute.name.local_name, node),
                            Err(err) => panic!("Error parsing: {} error: {:?}", attribute.value, err)
                        };
                    }
                    entity_stack.push(entity_id);
                }
                XmlEvent::EndElement { name: type_name } => {
                    entity_stack.pop();
                }
                XmlEvent::Error(e) => {
                    println!("Error: {}", e);
                    break;
                }
                _ => {}
            }
        }
    }

    fn entity_to_xml<T: Write>(&self, entity_id: &EntityId, writer: &mut xml::writer::EventWriter<T>) {
        let entity = self.entities.get(entity_id).unwrap();
        let type_name = xml::name::Name::local(&entity.type_name);
        let attrs: Vec<xml::attribute::OwnedAttribute> = entity.properties.iter().map(|(name, prop)| {
            xml::attribute::OwnedAttribute {
                name: xml::name::OwnedName::local(name.to_string()),
                value: prop.expression.borrow().to_string()
            }
        }).collect();
        writer.write(xml::writer::events::XmlEvent::StartElement {
            name: type_name.clone(),
            attributes: attrs.iter().map(|x| x.borrow()).collect(),
            namespace: &xml::namespace::Namespace::empty()
        });
        for e in &entity.children_ids {
            self.entity_to_xml(e, writer);
        }
        writer.write(xml::writer::events::XmlEvent::EndElement {
            name: type_name.clone()
        });
    }
    fn to_xml(&self) -> String {
        let mut buff = vec![];
        {
            let mut writer = xml::writer::EventWriter::new(&mut buff);
            writer.write(xml::writer::events::XmlEvent::StartDocument {
                version: xml::common::XmlVersion::Version11,
                encoding: None,
                standalone: None
            });
            self.entity_to_xml(&self.root, &mut writer);
        }
        String::from_utf8(buff).unwrap()
    }
}

fn event_reader_from_file(path: &Path) -> EventReader<BufReader<File>> {
    let file = File::open(path).unwrap();
    let file = BufReader::new(file);

    EventReader::new(file)
}

impl ToString for Document {
    fn to_string(&self) -> String {
        self.to_xml()
    }
}

#[test]
fn test_property_get() {
    let doc = Document::from_string(r#"<Entity name="tmp" x="5.0" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "x"), Ok(pon_parse::body("5.0").unwrap()));
}

#[test]
fn test_property_set() {
    let mut doc = Document::from_string(r#"<Entity name="tmp" x="5.0" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    {
        doc.set_property(&ent, "x", Pon::Integer(9));
    }
    assert_eq!(doc.get_property_value(&ent, "x"), Ok(pon_parse::body("9").unwrap()));
}

#[test]
fn test_property_reference_straight() {
    let doc = Document::from_string(r#"<Entity name="tmp" x="5.0" y="@this.x" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "y"), Ok(pon_parse::body("5.0").unwrap()));
}

#[test]
fn test_property_reference_object() {
    let doc = Document::from_string(r#"<Entity name="tmp" x="5.0" y="{ some: @this.x }" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "y"), Ok(pon_parse::body("{ some: 5.0 }").unwrap()));
}

#[test]
fn test_property_reference_transfer() {
    let doc = Document::from_string(r#"<Entity name="tmp" x="5.0" y="something @this.x" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "y"), Ok(pon_parse::body("something 5.0").unwrap()));
}

#[test]
fn test_property_reference_array() {
    let doc = Document::from_string(r#"<Entity name="tmp" x="5.0" y="[@this.x]" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "y"), Ok(pon_parse::body("[5.0]").unwrap()));
}

#[test]
fn test_property_reference_bad_ref() {
    let doc = Document::from_string(r#"<Entity name="tmp" x="5.0" y="@what.x" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "y"), Err(DocError::NoSuchProperty("y".to_string())));
}

#[test]
fn test_property_reference_parent() {
    let doc = Document::from_string(r#"<Entity x="5.0"><Entity name="tmp" y="@parent.x" /></Entity>"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    assert_eq!(doc.get_property_value(&ent, "y"), Ok(Pon::Float(5.0)));
}

#[test]
fn test_property_reference_update() {
    let mut doc = Document::from_string(r#"<Entity name="tmp" x="5.0" y="@this.x" />"#);
    let ent = doc.get_entity_by_name("tmp").unwrap();
    {
        let cascades = doc.set_property(&ent, "x", Pon::Integer(9)).ok().unwrap();
        assert_eq!(cascades.len(), 2);
        assert_eq!(cascades[0], PropRef { entity_id: ent, property_key: "x".to_string() });
        assert_eq!(cascades[1], PropRef { entity_id: ent, property_key: "y".to_string() });
    }
    assert_eq!(doc.get_property_value(&ent, "y"), Ok(pon_parse::body("9").unwrap()));
}

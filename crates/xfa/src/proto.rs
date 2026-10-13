//! Prototypes (XFA 3.3 "Prototypes"): an element with `use="#id"` or `usehref=".#som(…)"` takes
//! the attributes, content and children of the element it names, and overrides what it
//! declares itself. Designer's style sheets are written this way: a field says which style it
//! has and its size, border, font and margins are the style's.
//!
//! [`expand`] writes the template again with every reference resolved, so the parser reads
//! plain elements. Only references into the same template are followed.

use std::collections::HashSet;

use roxmltree::Node;

/// Deepest element nesting written.
const MAX_DEPTH: usize = 64;
/// Longest chain of prototypes that name prototypes.
const MAX_CHAIN: usize = 16;
/// Most bytes written: a reference repeats its prototype, so a hostile template could grow
/// without bound.
const MAX_OUTPUT: usize = 64 << 20;
/// Most elements looked at to resolve one reference.
const MAX_SEARCH: usize = 200_000;

/// Elements whose children are a list, or one of several alternatives: a referencing element
/// that has any replaces the prototype's, instead of overriding them one by one.
fn replaces_children(tag: &str) -> bool {
    matches!(tag, "items" | "value" | "exData" | "script" | "desc" | "extras" | "traversal" | "variables")
}

/// The containers a `#subform`-less SOM step looks through: they have no name of their own.
fn transparent(n: Node) -> bool {
    n.tag_name().name() == "proto" || n.tag_name().name() == "subform" && n.attribute("name").is_none_or(str::is_empty)
}

/// The template again with its prototype references resolved, or `None` when it has none (or
/// resolving them would write more than the budget allows: the template is then read as it is).
pub(crate) fn expand(template: Node) -> Option<String> {
    if !template.descendants().any(|n| n.is_element() && (n.attribute("use").is_some() || n.attribute("usehref").is_some())) {
        return None;
    }
    let mut w = Writer { template, out: String::new(), failed: false };
    w.element(&[template], None, 0);
    (!w.failed).then_some(w.out)
}

struct Writer<'a, 'd> {
    template: Node<'a, 'd>,
    out: String,
    failed: bool,
}

fn escape(s: &str, attribute: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\n' | '\r' | '\t' if attribute => out.push_str(&format!("&#{};", u32::from(c))),
            _ => out.push(c),
        }
    }
}

impl<'a, 'd> Writer<'a, 'd> {
    /// The element a reference names: `#id`, or `som(expr)` after a `usehref`'s `#`. `None` for
    /// another document, or a name nothing has.
    fn target(&self, n: Node<'a, 'd>) -> Option<Node<'a, 'd>> {
        let fragment = match (n.attribute("usehref").filter(|h| !h.is_empty()), n.attribute("use")) {
            (Some(href), _) => {
                let (document, fragment) = href.split_once('#')?;
                if !matches!(document.trim(), "" | ".") {
                    return None;
                }
                fragment
            }
            (None, Some(id)) => id.strip_prefix('#').unwrap_or(id),
            (None, None) => return None,
        };
        let fragment = fragment.trim();
        let found = match fragment.strip_prefix("som(").and_then(|s| s.strip_suffix(')')) {
            Some(expr) => self.som(expr.trim()),
            None => self.template.descendants().take(MAX_SEARCH).find(|d| d.is_element() && d.attribute("id") == Some(fragment)),
        };
        // An element is never its own prototype, and a prototype is of the same class.
        found.filter(|t| *t != n && t.tag_name().name() == n.tag_name().name())
    }

    /// A SOM expression from the template down: `$template.#subform.styles.Heading[1]`. A step
    /// is a name or a `#class`, with an optional index; nameless subforms and `proto` elements
    /// are looked through.
    fn som(&self, expr: &str) -> Option<Node<'a, 'd>> {
        let mut steps = expr.split('.').map(str::trim).peekable();
        while steps.peek().is_some_and(|s| matches!(*s, "$template" | "template" | "xfa" | "$xfa" | "template[0]" | "xfa[0]")) {
            steps.next();
        }
        let mut at = self.template;
        let mut budget = MAX_SEARCH;
        for step in steps {
            let (name, index) = match step.split_once('[') {
                Some((name, index)) => (name, index.trim_end_matches(']').trim().parse::<usize>().ok()?),
                None => (step, 0),
            };
            let mut matches = Vec::new();
            Self::step(at, name, &mut matches, &mut budget, 0);
            at = *matches.get(index)?;
        }
        (at != self.template).then_some(at)
    }

    fn step(at: Node<'a, 'd>, name: &str, out: &mut Vec<Node<'a, 'd>>, budget: &mut usize, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for c in at.children().filter(Node::is_element) {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let hit = match name.strip_prefix('#') {
                Some(class) => c.tag_name().name() == class,
                None => c.attribute("name") == Some(name),
            };
            if hit {
                out.push(c);
            } else if transparent(c) {
                Self::step(c, name, out, budget, depth + 1);
            }
        }
    }

    /// `n` and the prototypes it takes after, nearest first.
    fn chain(&self, n: Node<'a, 'd>) -> Vec<Node<'a, 'd>> {
        let mut chain = vec![n];
        let mut seen = HashSet::from([n.id()]);
        let mut at = n;
        while chain.len() < MAX_CHAIN
            && let Some(t) = self.target(at)
            && seen.insert(t.id())
        {
            chain.push(t);
            at = t;
        }
        chain
    }

    /// Write the element `layers` make together: `layers[0]` over `layers[1]` over …, each
    /// already an element of the same class. `parent` is the element written around it (for
    /// namespace declarations).
    fn element(&mut self, layers: &[Node<'a, 'd>], parent: Option<Node<'a, 'd>>, depth: usize) {
        if self.failed {
            return;
        }
        // Every layer may itself take after a prototype.
        let layers: Vec<Node<'a, 'd>> = layers.iter().flat_map(|l| self.chain(*l)).take(MAX_CHAIN).collect();
        let Some(&top) = layers.first() else { return };
        if depth > MAX_DEPTH || self.out.len() > MAX_OUTPUT {
            self.failed = true;
            return;
        }
        let tag = top.tag_name().name();
        let prefix =
            |n: Node, ns: Option<&str>| ns.and_then(|ns| n.lookup_prefix(ns)).filter(|p| !p.is_empty()).map(|p| format!("{p}:")).unwrap_or_default();
        let qname = format!("{}{tag}", prefix(top, top.tag_name().namespace()));
        self.out.push('<');
        self.out.push_str(&qname);
        // Namespaces: those in scope here that the element written around this one lacks.
        let inherited: Vec<(Option<&str>, &str)> = parent.map(|p| p.namespaces().map(|ns| (ns.name(), ns.uri())).collect()).unwrap_or_default();
        let mut declared: HashSet<Option<&str>> = HashSet::new();
        for layer in &layers {
            for ns in layer.namespaces() {
                if !inherited.contains(&(ns.name(), ns.uri())) && declared.insert(ns.name()) {
                    match ns.name() {
                        Some(name) => self.out.push_str(&format!(" xmlns:{name}=\"")),
                        None => self.out.push_str(" xmlns=\""),
                    }
                    escape(ns.uri(), true, &mut self.out);
                    self.out.push('"');
                }
            }
        }
        // Attributes: the nearest layer that has one wins. A prototype's `id` is its own.
        let mut written: HashSet<String> = HashSet::new();
        for (i, layer) in layers.iter().enumerate() {
            for a in layer.attributes() {
                let name = format!("{}{}", prefix(*layer, a.namespace()), a.name());
                if matches!(a.name(), "use" | "usehref") || i > 0 && a.name() == "id" || !written.insert(name.clone()) {
                    continue;
                }
                self.out.push(' ');
                self.out.push_str(&name);
                self.out.push_str("=\"");
                escape(a.value(), true, &mut self.out);
                self.out.push('"');
            }
        }
        self.out.push('>');
        // Content: where a layer has text or (for lists and alternatives) children of its own,
        // the layers below give nothing.
        let own_content = |n: &Node| n.children().any(|c| c.is_element() || c.is_text() && c.text().is_some_and(|t| !t.trim().is_empty()));
        let cut = if replaces_children(tag) { layers.iter().position(own_content).map_or(layers.len(), |i| i + 1) } else { layers.len() };
        let layers = layers.get(..cut).unwrap_or(&layers);
        // Text is the nearest layer's that has any; a layer's children keep their order with it.
        let has_text = |l: &Node| l.children().any(|c| c.is_text() && c.text().is_some_and(|t| !t.trim().is_empty()));
        let text_layer = layers.iter().position(has_text).unwrap_or(0);
        // Children: each layer's, in its order; one a nearer layer has too (the same class, at
        // the same position among that class) is written once, the nearer over the farther.
        let nth = |layer: Node<'a, 'd>, tag: &str, n: usize| layer.children().filter(|c| c.is_element() && c.tag_name().name() == tag).nth(n);
        // In a `ui` only one widget is meant: a nearer layer's replaces the prototype's.
        let widget = |c: &Node| !matches!(c.tag_name().name(), "picture" | "extras");
        let chosen_widget = layers.iter().find_map(|l| l.children().find(|c| c.is_element() && widget(c)).map(|c| c.tag_name().name().to_string()));
        let mut done: HashSet<(String, usize)> = HashSet::new();
        for (i, layer) in layers.iter().enumerate() {
            let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for c in layer.children() {
                if c.is_text() && i == text_layer {
                    escape(c.text().unwrap_or_default(), false, &mut self.out);
                }
                if !c.is_element() {
                    continue;
                }
                let ctag = c.tag_name().name();
                let n = {
                    let k = counts.entry(ctag).or_insert(0);
                    *k += 1;
                    *k - 1
                };
                if ctag == "proto" || tag == "ui" && widget(&c) && chosen_widget.as_deref() != Some(ctag) {
                    continue;
                }
                if !done.insert((ctag.to_string(), n)) {
                    continue;
                }
                let stack: Vec<Node<'a, 'd>> =
                    std::iter::once(c).chain(layers.iter().skip(i + 1).filter_map(|below| nth(*below, ctag, n))).take(MAX_CHAIN).collect();
                self.element(&stack, Some(top), depth + 1);
            }
        }
        self.out.push_str("</");
        self.out.push_str(&qname);
        self.out.push('>');
    }
}

// The loose modules look up the page's elements by selector: index.html, not the type checker,
// knows what each one is, so a lookup by selector is untyped. Typed modules cast what they look up.
interface ParentNode {
  querySelector(selectors: string): any;
  querySelectorAll(selectors: string): NodeListOf<any>;
}
interface Element {
  closest(selectors: string): any;
}

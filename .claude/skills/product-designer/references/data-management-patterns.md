# Data Management UI Patterns

## Master-Detail View

**Description**: List of records on the left/main area, detail panel on the right or as a separate page. Clicking a row reveals full details.

**When to use**: Browsing collections where users frequently switch between items (email, records, policies).

**Layout**:
```
┌─────────────────────┬───────────────────┐
│ List (filterable)    │ Detail Panel      │
│ ┌─────────────────┐ │ Title             │
│ │ Item 1 (active) │ │ Tabs: Info|History│
│ │ Item 2          │ │ Content area      │
│ │ Item 3          │ │                   │
│ └─────────────────┘ │ [Actions]         │
└─────────────────────┴───────────────────┘
```

**Pros**: Fast context switching, minimal navigation.
**Cons**: Cramped on small screens, complex to implement responsively.
**your design system**: Side panel uses `--sds-bg-card`, list uses `--sds-bg-page`. Active item uses `--sds-nav-item-active-bg/text`.

---

## Data Table with Inline Actions

**Description**: Tabular display with sortable columns, filterable rows, and row-level action buttons.

**When to use**: Structured data with many attributes (users, policies, connections, audit logs).

**Features**:
- Column sorting (click header to sort)
- Column resizing (drag column borders)
- Row selection (checkbox column)
- Inline actions (Edit, Delete buttons in last column)
- Bulk actions toolbar (appears when rows selected)
- Pagination or infinite scroll

**Pros**: Familiar, scannable, handles large datasets.
**Cons**: Dense, can overwhelm with too many columns.
**your design system**: Table headers use `--sds-bg-subtle`, borders use `--sds-border-default`, row hover uses warm-gray-050.

---

## Filter Panel

**Description**: Side panel or inline bar with filter controls (dropdowns, checkboxes, date pickers, search).

**When to use**: Lists with 50+ items where users need to narrow results by multiple criteria.

**Variations**:
| Type | Placement | Best For |
|------|-----------|----------|
| Inline filter bar | Above table | 1-4 simple filters |
| Side filter panel | Left of table | 5+ filters or complex criteria |
| Filter chips | Above table, removable | Showing active filters |
| Saved filters/views | Dropdown above table | Repeated filter combinations |

**Pros**: Reduces cognitive load, makes large datasets manageable.
**Cons**: Takes screen space, can be complex to build.
**your design system**: Filter panel uses `--sds-bg-surface`, active filter chips use `--sds-interactive-primary-subtle` with `--sds-interactive-primary` text.

---

## Bulk Operations

**Description**: Select multiple items and apply an action to all at once (delete, export, tag, move).

**When to use**: Any list where users manage items in groups.

**Flow**:
1. User checks individual rows or "Select all"
2. Bulk action toolbar appears (slide down or overlay)
3. Toolbar shows: count selected + action buttons
4. Confirm destructive bulk actions with dialog

**Toolbar pattern**:
```
┌──────────────────────────────────────────┐
│ ☑ 12 selected   [Export] [Tag] [Delete]  │
└──────────────────────────────────────────┘
```

**Pros**: Efficient for batch operations.
**Cons**: "Select all" across pages is tricky, destructive bulk actions are risky.
**your design system**: Toolbar uses `--sds-interactive-primary-subtle` bg. Delete uses danger button variant.

---

## Saved Views / Column Configuration

**Description**: Let users customize which columns are visible and save named views.

**When to use**: Data tables with 8+ possible columns where different users care about different fields.

**Features**:
- Column visibility toggle (dropdown with checkboxes)
- Column reorder (drag and drop)
- Save as named view
- Share views with team

**Pros**: Personalized experience, reduces noise.
**Cons**: Adds complexity, saved views need storage.

---

## Import / Export

**Description**: Upload data from files (CSV, JSON, Excel) or download data in various formats.

**When to use**: Any data management app where users move data in/out.

**Import flow**:
1. Upload file (drag & drop zone)
2. Preview & map columns
3. Validate (show errors inline)
4. Confirm & import
5. Show results summary

**Export flow**:
1. Select format (CSV, JSON, Excel, PDF)
2. Choose columns / filters to include
3. Download or email link

**Pros**: Essential for data interop.
**Cons**: Complex validation, error handling for malformed files.
**your design system**: Upload zone uses dashed `--sds-border-default` border. Validation errors use `--sds-status-error-*` tokens.

---

## Search

**Description**: Free-text search with optional filters, autocomplete, and result highlighting.

**When to use**: Any dataset where users know what they're looking for by name or keyword.

**Variations**:
| Type | Description | Best For |
|------|-------------|----------|
| Simple search bar | Text input above table | Small datasets, single-field search |
| Advanced search | Multi-field with operators (is, contains, starts with) | Power users, complex queries |
| Global search | App-wide search across all data types | Large apps with many data types |
| Typeahead/autocomplete | Dropdown suggestions as user types | Selecting from known values |

**Pros**: Fast for known-item lookup.
**Cons**: Full-text search infrastructure needed for large datasets.

---

## CRUD Workflow Pattern

**Description**: The standard Create → Read → Update → Delete lifecycle for any entity.

**Standard flow**:
```
List View ──[+ Create]──→ Create Form ──[Save]──→ Detail View
    │                                                  │
    └──[Click row]──→ Detail View ──[Edit]──→ Edit Form
                          │
                          └──[Delete]──→ Confirm Dialog ──→ List View
```

**Best practices**:
- Create and Edit use the same form component (prefilled for edit)
- Delete always requires confirmation dialog
- After create/edit, navigate to the detail view (not back to list)
- Show success toast after save
- Preserve list filters/sort when returning from detail

**your design system**: Create button = primary, Edit = secondary, Delete = danger or danger-outline.

---

## Role-Based Access Indicators

**Description**: Visual indicators showing what the current user can and cannot do.

**Patterns**:
| Pattern | Use Case |
|---------|----------|
| Disabled buttons with tooltip | User sees the action exists but can't use it |
| Hidden actions | User doesn't need to know the action exists |
| Read-only badge | Show "View only" indicator at page level |
| Permission-required inline | "Contact admin for edit access" |

**When to use**: Multi-role applications with distinct permission levels.

**your design system**: Disabled uses `--sds-text-disabled`. Read-only badge uses `--sds-status-neutral-*` tokens.

---

## Data Lineage / Relationship Visualization

**Description**: Show how data entities relate to each other (dependencies, references, flow).

**When to use**: Complex data models where understanding relationships matters (policy → data plane → connection).

**Patterns**:
- Breadcrumb trail showing hierarchy
- Dependency graph (nodes and edges)
- Related items section on detail pages
- Impact analysis ("Deleting this will affect 3 data planes")

**your design system**: Relationship links use `--sds-text-link`. Impact warnings use `--sds-status-warning-*`.

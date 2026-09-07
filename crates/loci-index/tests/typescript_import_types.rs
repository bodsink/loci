//! `import('mod').T[]` is valid TypeScript. The bundled grammar does not treat
//! the import form as a `primary_type`, so the `[]` becomes a parse error and
//! the file is reported `parse_partial`.
//!
//! Copied from the three files that stayed partial on a real Go/React project
//! after the JSX-ampersand and keyword-member passes.

use loci_graph::NodeLabel;
use loci_index::IndexOptions;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn serial() -> MutexGuard<'static, ()> {
    static DATA_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let dir = DATA_DIR.get_or_init(|| tempfile::tempdir().expect("data dir"));
    std::env::set_var("LOCI_DATA_DIR", dir.path());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, contents).expect("write");
}

fn index(root: &Path, name: &str) -> loci_index::IndexReport {
    loci_index::index_repository(
        root,
        &IndexOptions {
            name: Some(name.to_string()),
            full: true,
            hybrid_lsp: false,
        },
    )
    .expect("index")
}

fn nodes(project: &str) -> Vec<(NodeLabel, String)> {
    let (_, store) = loci_index::open_project(project).expect("open");
    store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| (n.label, n.name.clone()))
        .collect()
}

const BILLING: &str = "\
export const getBillings = async (
  invoice_category: string = '',
  start_date?: string,
): Promise<void> => {}

export const getBillingWhatsAppDeliveries = async (id: string) => {
  const response = await api.get<{ data: import('@/types').WAMessage[] }>(
    `/billings/${id}/whatsapp-deliveries`,
  )
  return response.data.data ?? []
}
";

const WORK_ORDER: &str = "\
export interface CreateCategoryRequest {
  required_fields?: import('@/types/workOrder').WorkOrderCategoryField[]
}
";

const ONUS: &str = "\
export function useSearchONUsByCustomerName(q: string) {
  return useQuery({
    queryFn: async () => {
      const res = await api.get<{ data: import('../services/onuService').ONUNameSearchResult[] }>('/onus/search-by-name')
      return res.data.data ?? []
    },
  })
}
";

#[test]
fn import_type_arrays_are_not_reported_as_partial() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "src/billingService.ts", BILLING);
    write(root.path(), "src/workOrderService.ts", WORK_ORDER);
    write(root.path(), "src/useONUs.ts", ONUS);

    let report = index(root.path(), "ts-import-types");

    assert_eq!(
        report.files_parse_partial, 0,
        "import('mod').T[] must parse: {:?}",
        report.parse_partial_examples
    );

    let found = nodes("ts-import-types");
    for expected in [
        "getBillings",
        "getBillingWhatsAppDeliveries",
        "CreateCategoryRequest",
        "useSearchONUsByCustomerName",
    ] {
        assert!(
            found.iter().any(|(_, name)| name == expected),
            "{expected} must reach the graph: {found:?}"
        );
    }
}

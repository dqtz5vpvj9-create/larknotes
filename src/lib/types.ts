export interface DocMeta {
  doc_id: string;
  title: string;
  doc_type: string;
  url: string;
  owner_name: string;
  created_at: string;
  updated_at: string;
  local_path: string | null;
  content_hash: string | null;
  sync_status: SyncStatus;
}

export type SyncStatus =
  | { type: "Synced" }
  | { type: "LocalModified" }
  | { type: "Syncing" }
  | { type: "Conflict" }
  | { type: "Error"; message: string }
  | { type: "New" };

export interface AuthStatus {
  logged_in: boolean;
  user_name: string | null;
  expires_at: string | null;
  needs_refresh: boolean;
}

export interface AppConfig {
  workspace_dir: string;
  editor_command: string;
  lark_cli_path: string;
  sync_debounce_ms: number;
  auto_sync: boolean;
}

export interface SyncStatusUpdate {
  doc_id: string;
  status: SyncStatus;
  title: string | null;
}

export interface SyncHistoryEntry {
  id: number;
  doc_id: string;
  action: string;
  content_hash: string | null;
  created_at: string;
}

export interface VersionSnapshot {
  id: number;
  doc_id: string;
  content: string;
  content_hash: string;
  created_at: string;
}

export type SortField = "updated_at" | "title" | "sync_status";
export type SortDirection = "asc" | "desc";

export function syncStatusLabel(status: SyncStatus): string {
  switch (status.type) {
    case "Synced":
      return "已同步";
    case "LocalModified":
      return "本地已修改";
    case "Syncing":
      return "同步中...";
    case "Conflict":
      return "冲突";
    case "Error":
      return "错误";
    case "New":
      return "新建";
  }
}

export function syncStatusColor(type: string): string {
  switch (type) {
    case "Synced": return "var(--c-green)";
    case "Syncing": return "var(--c-blue)";
    case "LocalModified": return "var(--c-amber)";
    case "Conflict": return "var(--c-red)";
    case "Error": return "var(--c-red)";
    default: return "var(--c-text-tertiary)";
  }
}

/** Sort priority for sync status (lower = higher priority) */
function syncStatusPriority(type: string): number {
  switch (type) {
    case "Conflict": return 0;
    case "Error": return 1;
    case "Syncing": return 2;
    case "LocalModified": return 3;
    case "New": return 4;
    case "Synced": return 5;
    default: return 6;
  }
}

export function sortDocs(
  docs: DocMeta[],
  field: SortField,
  direction: SortDirection,
): DocMeta[] {
  return [...docs].sort((a, b) => {
    let cmp = 0;
    switch (field) {
      case "title":
        cmp = (a.title || "").localeCompare(b.title || "", "zh-CN");
        break;
      case "updated_at":
        cmp = (a.updated_at || "").localeCompare(b.updated_at || "");
        break;
      case "sync_status":
        cmp = syncStatusPriority(a.sync_status.type) - syncStatusPriority(b.sync_status.type);
        break;
    }
    return direction === "asc" ? cmp : -cmp;
  });
}

export function formatRelativeTime(iso: string): string {
  if (!iso) return "";
  try {
    const d = new Date(iso);
    const now = new Date();
    const diff = now.getTime() - d.getTime();
    if (diff < 60_000) return "刚刚";
    if (diff < 3600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
    if (diff < 86400_000) {
      const yesterday = new Date(now);
      yesterday.setDate(yesterday.getDate() - 1);
      if (d.toDateString() === yesterday.toDateString()) return "昨天";
      return `${Math.floor(diff / 3600_000)} 小时前`;
    }
    if (diff < 604800_000) return `${Math.floor(diff / 86400_000)} 天前`;
    return d.toLocaleDateString("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

/** Split text into segments for highlight rendering */
export function highlightSegments(text: string, query: string): { text: string; match: boolean }[] {
  if (!query) return [{ text, match: false }];
  const lower = text.toLowerCase();
  const qLower = query.toLowerCase();
  const segments: { text: string; match: boolean }[] = [];
  let lastIndex = 0;
  let idx = lower.indexOf(qLower);
  while (idx !== -1) {
    if (idx > lastIndex) segments.push({ text: text.slice(lastIndex, idx), match: false });
    segments.push({ text: text.slice(idx, idx + query.length), match: true });
    lastIndex = idx + query.length;
    idx = lower.indexOf(qLower, lastIndex);
  }
  if (lastIndex < text.length) segments.push({ text: text.slice(lastIndex), match: false });
  return segments.length ? segments : [{ text, match: false }];
}

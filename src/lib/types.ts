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

export function syncStatusBadge(status: SyncStatus): string {
  switch (status.type) {
    case "Synced":
      return "badge-synced";
    case "Syncing":
      return "badge-syncing";
    case "LocalModified":
      return "badge-modified";
    case "Conflict":
      return "badge-conflict";
    case "Error":
      return "badge-error";
    default:
      return "badge-synced";
  }
}

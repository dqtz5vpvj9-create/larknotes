import { invoke } from "@tauri-apps/api/core";
import type { AuthStatus, DocMeta, AppConfig, SyncHistoryEntry, VersionSnapshot } from "./types";

export const getAuthStatus = () =>
  invoke<AuthStatus>("get_auth_status");

export const searchDocs = (query: string) =>
  invoke<DocMeta[]>("search_docs", { query });

export const searchDocsLocal = (query: string) =>
  invoke<DocMeta[]>("search_docs_local", { query });

export const createDoc = (title: string) =>
  invoke<DocMeta>("create_doc", { title });

export const openDocInEditor = (docId: string) =>
  invoke<void>("open_doc_in_editor", { docId });

export const getDocList = () =>
  invoke<DocMeta[]>("get_doc_list");

export const getAppConfig = () =>
  invoke<AppConfig>("get_app_config");

export const setEditor = (editor: string) =>
  invoke<void>("set_editor", { editor });

export const setWorkspace = (path: string) =>
  invoke<void>("set_workspace", { path });

export const detectEditors = () =>
  invoke<[string, string][]>("detect_editors");

export const pickFolder = () =>
  invoke<string | null>("pick_folder");

export const manualSync = (docId: string) =>
  invoke<void>("manual_sync", { docId });

export const importDoc = (docId: string) =>
  invoke<DocMeta>("import_doc", { docId });

export const deleteDoc = (docId: string) =>
  invoke<void>("delete_doc", { docId });

export const revealInExplorer = (docId: string) =>
  invoke<void>("reveal_in_explorer", { docId });

export const getSyncHistory = (docId: string) =>
  invoke<SyncHistoryEntry[]>("get_sync_history", { docId });

export const getSnapshots = (docId: string) =>
  invoke<VersionSnapshot[]>("get_snapshots", { docId });

export const restoreSnapshot = (docId: string, snapshotId: number) =>
  invoke<void>("restore_snapshot", { docId, snapshotId });

export const quickNote = () =>
  invoke<DocMeta>("quick_note");

export const getAutostartStatus = () =>
  invoke<boolean>("get_autostart_status");

export const setAutostart = (enabled: boolean) =>
  invoke<void>("set_autostart", { enabled });

export const pullDoc = (docId: string) =>
  invoke<DocMeta>("pull_doc", { docId });

export const setSyncDebounce = (ms: number) =>
  invoke<void>("set_sync_debounce", { ms });

export const setAutoSync = (enabled: boolean) =>
  invoke<void>("set_auto_sync", { enabled });

export const setLarkCliPath = (path: string) =>
  invoke<void>("set_lark_cli_path", { path });

export const openLoginUrl = () =>
  invoke<string>("open_login_url");

export const resolveConflict = (docId: string, resolution: string) =>
  invoke<DocMeta>("resolve_conflict", { docId, resolution });

export const getConflictDiff = (docId: string) =>
  invoke<[string, string]>("get_conflict_diff", { docId });

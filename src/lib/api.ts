import { invoke } from "@tauri-apps/api/core";
import type { AuthStatus, DocMeta, AppConfig, SyncHistoryEntry, VersionSnapshot } from "./types";

export const getAuthStatus = () =>
  invoke<AuthStatus>("get_auth_status");

export const searchDocs = (query: string) =>
  invoke<DocMeta[]>("search_docs", { query });

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

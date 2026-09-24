<script lang="ts">
  // <FileAttachments target_kind={...} target_id={...} />
  //
  // Single component for all four target kinds (Subject/Job/Step/
  // Event). Lists existing attachments + offers drag-drop + file
  // picker upload. Per design Q5 lives in apps/web/src/content/
  // alongside other content-domain components.

  import {
    deleteFile,
    downloadHref,
    formatBytes,
    isImage,
    listFilesFor,
    uploadFile,
    type FileRef,
    type ResourceKind,
  } from './files';

  type Props = Readonly<{
    targetKind: ResourceKind;
    targetId: string;
    /// Whether the current viewer is permitted to upload/delete.
    /// Server still enforces; this hides the affordance for read-only
    /// roles. Defaults to true so the simple case "just render the
    /// uploader" doesn't need a prop.
    canEdit?: boolean;
  }>;

  let { targetKind, targetId, canEdit = true }: Props = $props();

  let files = $state<ReadonlyArray<FileRef>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  // True when the deployment hasn't configured the file-storage
  // surface (boss-content-api `[files]` block missing). The api
  // returns 503 in that case; we render a quiet "not available"
  // message instead of a generic error.
  let unconfigured = $state(false);
  let uploading = $state(false);
  let dragOver = $state(false);

  $effect(() => {
    void load(targetKind, targetId);
  });

  async function load(kind: ResourceKind, id: string): Promise<void> {
    loading = true;
    try {
      const result = await listFilesFor(kind, id);
      if (result.kind === 'unconfigured') {
        unconfigured = true;
        files = [];
      } else {
        unconfigured = false;
        files = result.files;
      }
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function handleFiles(picked: FileList | File[] | null): Promise<void> {
    if (!picked) return;
    const arr = Array.from(picked);
    if (arr.length === 0) return;
    uploading = true;
    try {
      // Sequential upload — keeps the order predictable + makes
      // policy denials surface one at a time. v1 doesn't need parallel.
      for (const f of arr) {
        await uploadFile(targetKind, targetId, f);
      }
      await load(targetKind, targetId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      uploading = false;
    }
  }

  function onPick(e: Event): void {
    const input = e.currentTarget as HTMLInputElement;
    void handleFiles(input.files);
    // Clear so picking the same file twice in a row triggers `change`.
    input.value = '';
  }

  function onDrop(e: DragEvent): void {
    e.preventDefault();
    dragOver = false;
    void handleFiles(e.dataTransfer?.files ?? null);
  }

  function onDragOver(e: DragEvent): void {
    e.preventDefault();
    dragOver = true;
  }

  function onDragLeave(): void {
    dragOver = false;
  }

  async function onDelete(id: string): Promise<void> {
    if (!confirm('Detach this file? Bytes are kept for 30 days.')) return;
    try {
      await deleteFile(id);
      await load(targetKind, targetId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<div class="files-root">
  {#if loading}
    <p class="files-empty">Loading attachments…</p>
  {:else if unconfigured}
    <p class="files-empty">
      File attachments aren't enabled in this deployment — the
      <code>[files]</code> block is missing from the
      boss-content-api config. Configure object storage to turn
      this surface on.
    </p>
  {:else if error}
    <p class="files-error">Couldn't load attachments — {error}</p>
  {:else if files.length === 0}
    <p class="files-empty">No attachments yet.</p>
  {:else}
    <ul class="files-list">
      {#each files as f (f.id)}
        <li class="files-item">
          {#if isImage(f.mime)}
            <a class="files-thumb" href={downloadHref(f.id)} target="_blank" rel="noopener">
              <img src={downloadHref(f.id)} alt={f.filename} />
            </a>
          {:else}
            <a class="files-icon" href={downloadHref(f.id)} target="_blank" rel="noopener">
              <span aria-hidden="true">📎</span>
            </a>
          {/if}
          <div class="files-meta">
            <a class="files-filename" href={downloadHref(f.id)} target="_blank" rel="noopener">
              {f.filename}
            </a>
            <div class="files-sub">
              {formatBytes(f.size_bytes)} · {f.mime} · uploaded by {f.uploaded_by}
            </div>
          </div>
          {#if canEdit}
            <button class="files-delete" type="button" onclick={() => onDelete(f.id)}>
              Detach
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if canEdit && !unconfigured}
    <label
      class="files-drop"
      class:files-drop-over={dragOver}
      ondrop={onDrop}
      ondragover={onDragOver}
      ondragleave={onDragLeave}
    >
      <input type="file" multiple onchange={onPick} disabled={uploading} hidden />
      <span class="files-drop-text">
        {#if uploading}
          Uploading…
        {:else}
          Drop files here, or <span class="files-drop-link">click to upload</span>
        {/if}
      </span>
    </label>
  {/if}
</div>

<style>
  .files-root {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .files-empty,
  .files-error {
    color: var(--static);
    font-size: 0.9rem;
  }
  .files-error {
    color: var(--err);
  }
  .files-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .files-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--ink);
  }
  .files-thumb img {
    width: 48px;
    height: 48px;
    object-fit: cover;
    border-radius: 4px;
    display: block;
  }
  .files-icon {
    width: 48px;
    height: 48px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 1.5rem;
    background: var(--ink-raised);
    border-radius: 4px;
    text-decoration: none;
  }
  .files-meta {
    flex: 1;
    min-width: 0;
  }
  .files-filename {
    color: var(--text);
    font-weight: 500;
    text-decoration: none;
  }
  .files-filename:hover {
    text-decoration: underline;
  }
  .files-sub {
    font-size: 0.8rem;
    color: var(--static);
    margin-top: 2px;
  }
  .files-delete {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--static);
    padding: 4px 10px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .files-delete:hover {
    color: var(--err);
    border-color: var(--err);
  }
  .files-drop {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    border: 2px dashed var(--border);
    border-radius: 6px;
    cursor: pointer;
    color: var(--static);
    transition: background-color 120ms ease, border-color 120ms ease;
  }
  .files-drop:hover,
  .files-drop-over {
    background: var(--ink-raised);
    border-color: var(--accent);
    color: var(--text);
  }
  .files-drop-link {
    color: var(--accent);
    text-decoration: underline;
  }
</style>

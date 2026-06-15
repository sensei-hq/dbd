/* localStorage-backed "projects" — no login, no backend. Each loaded diagram
   is saved under `dbd:diagram:<project>` with its share-link payload as the
   value (the `1.<base64url-gzip-json>` string). /projects scans these keys and
   renders a card per design; the payload is also the shareable URL fragment. */
import { encodeFragment, decodeFragment } from '$lib/design/fragment';
import { validateModel, type SchemaModel } from '$lib/design/model';

const PREFIX = 'dbd:diagram:';
export const keyFor = (project: string) => PREFIX + project;

const hasStorage = () => typeof localStorage !== 'undefined';

/** Persist a loaded model under its project name. Pass the existing fragment
    payload to avoid re-encoding; otherwise it's encoded from the model. */
export async function saveDiagram(model: SchemaModel, payload?: string): Promise<void> {
  if (!hasStorage()) return;
  const p = payload ?? (await encodeFragment(model));
  try {
    localStorage.setItem(keyFor(model.project.name), p);
  } catch {
    // quota / disabled storage — saving is best-effort, never fatal.
  }
}

export type SavedDiagram = { project: string; payload: string; model: SchemaModel };

/** All saved diagrams, decoded + validated, sorted by project name. */
export async function listDiagrams(): Promise<SavedDiagram[]> {
  if (!hasStorage()) return [];
  const out: SavedDiagram[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (!k || !k.startsWith(PREFIX)) continue;
    const payload = localStorage.getItem(k);
    if (!payload) continue;
    try {
      const res = validateModel(await decodeFragment(payload));
      if (res.ok) out.push({ project: k.slice(PREFIX.length), payload, model: res.model });
    } catch {
      // skip corrupt entries rather than breaking the whole list
    }
  }
  return out.sort((a, b) => a.project.localeCompare(b.project));
}

export function deleteDiagram(project: string): void {
  if (!hasStorage()) return;
  try {
    localStorage.removeItem(keyFor(project));
  } catch {
    // ignore
  }
}

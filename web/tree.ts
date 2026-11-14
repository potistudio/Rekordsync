// チェックボックス付きのプレイリストツリー。
// フォルダをチェックすると配下もまとめて切り替わり、配下が一部だけ
// 選ばれているフォルダは中間状態（indeterminate）で表示する。

import type { PlaylistRow } from "./backend";

interface Node {
  row: PlaylistRow;
  children: Node[];
  parent: Node | null;
  checkbox: HTMLInputElement;
}

export class CheckTree {
  private nodes: Node[] = [];
  private onChange: () => void;

  constructor(private container: HTMLElement, onChange: () => void = () => {}) {
    this.onChange = onChange;
  }

  setRows(rows: PlaylistRow[], checked: boolean, emptyText: string): void {
    this.container.replaceChildren();
    this.nodes = [];

    // 親はフォルダ「名」で参照される（エクスポート形式の仕様）。
    const folderByName = new Map<string, Node>();
    const all: Node[] = rows.map((row) => {
      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.checked = checked;
      return { row, children: [], parent: null, checkbox };
    });
    for (const n of all) {
      if (n.row.is_folder && !folderByName.has(n.row.name)) folderByName.set(n.row.name, n);
    }
    const roots: Node[] = [];
    for (const n of all) {
      const parent = n.row.parent ? folderByName.get(n.row.parent) : undefined;
      if (parent && parent !== n && !isAncestor(n, parent)) {
        n.parent = parent;
        parent.children.push(n);
      } else {
        roots.push(n);
      }
    }
    // フォルダを先に並べる（元の並び順は保つ）。
    const sortNodes = (list: Node[]) => {
      list.sort((a, b) => Number(b.row.is_folder) - Number(a.row.is_folder));
      list.forEach((n) => sortNodes(n.children));
    };
    sortNodes(roots);
    this.nodes = all;

    if (roots.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = emptyText;
      this.container.append(empty);
      return;
    }
    const ul = document.createElement("ul");
    ul.className = "tree-root";
    roots.forEach((n) => ul.append(this.render(n)));
    this.container.append(ul);
    roots.forEach(refreshDown);
  }

  private render(node: Node): HTMLLIElement {
    const li = document.createElement("li");
    const label = document.createElement("label");
    label.className = "tree-row";

    const caret = document.createElement("span");
    caret.className = "caret";
    if (node.children.length > 0) {
      caret.textContent = "▾";
      caret.addEventListener("click", (e) => {
        e.preventDefault();
        li.classList.toggle("collapsed");
      });
    }

    node.checkbox.addEventListener("change", () => {
      setDown(node, node.checkbox.checked);
      if (node.parent) refreshUp(node.parent);
      this.onChange();
    });

    const name = document.createElement("span");
    name.className = node.row.is_folder ? "name folder" : "name";
    name.textContent = node.row.name;

    const count = document.createElement("span");
    count.className = "count";
    count.textContent = node.row.is_folder
      ? `${node.children.length} 件`
      : `${node.row.track_count} 曲`;

    label.append(caret, node.checkbox, name, count);
    li.append(label);

    if (node.children.length > 0) {
      const ul = document.createElement("ul");
      node.children.forEach((c) => ul.append(this.render(c)));
      li.append(ul);
    }
    return li;
  }

  setAll(checked: boolean): void {
    for (const n of this.nodes) {
      n.checkbox.checked = checked;
      n.checkbox.indeterminate = false;
    }
    this.onChange();
  }

  /** チェック済みの UUID。配下が一部でも選ばれたフォルダも含める。 */
  checkedUuids(): string[] {
    return this.nodes
      .filter((n) => n.checkbox.checked || n.checkbox.indeterminate)
      .map((n) => n.row.uuid);
  }

  /** チェック済みのプレイリスト（フォルダ以外）の数。 */
  checkedPlaylistCount(): number {
    return this.nodes.filter((n) => !n.row.is_folder && n.checkbox.checked).length;
  }

  get size(): number {
    return this.nodes.length;
  }
}

function isAncestor(candidate: Node, of: Node): boolean {
  for (let p: Node | null = of.parent; p; p = p.parent) if (p === candidate) return true;
  return false;
}

function setDown(node: Node, checked: boolean): void {
  node.checkbox.checked = checked;
  node.checkbox.indeterminate = false;
  node.children.forEach((c) => setDown(c, checked));
}

/** 子の状態からフォルダ自身のチェック表示を決める。 */
function recompute(node: Node): void {
  if (node.children.length === 0) return;
  const all = node.children.every((c) => c.checkbox.checked && !c.checkbox.indeterminate);
  const some = node.children.some((c) => c.checkbox.checked || c.checkbox.indeterminate);
  node.checkbox.checked = all;
  node.checkbox.indeterminate = some && !all;
}

/** 葉から根へ向けて、部分木全体の表示を更新する（初期表示用）。 */
function refreshDown(node: Node): void {
  node.children.forEach(refreshDown);
  recompute(node);
}

/** 変更のあった位置から根まで、祖先の表示を更新する。 */
function refreshUp(node: Node): void {
  recompute(node);
  if (node.parent) refreshUp(node.parent);
}

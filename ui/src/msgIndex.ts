/**
 * 消息索引：把 tweb 的 `SlicedArray` 用在我们真正需要它的地方——**历史分页
 * 合并**。原来 `mergeMessages` 是手写的 `Map + sort`（每页 O(n log n)），
 * 而 tweb 的切片结构就是为"按 seq 降序、分块插入/去重"设计的，直接复用。
 *
 * 语义对齐（见 `vendor/tweb/slicedArray.ts` 的注释 `[[7,6,5],[4,3,2]]`）：
 * - `slices[0]` 是**最新**一块，块内降序；
 * - 新消息（更大的 seq）走 `unshift`，历史页（更小的 seq）走 `insertSlice`；
 * - 对外仍返回**升序数组**，与 `ChatMsg[]` 的既有消费方式保持一致。
 */

import SlicedArray from "./vendor/tweb/slicedArray";
import type { ChatMsg } from "./state";

export class MessageIndex {
  private readonly index = new SlicedArray<number>();
  private readonly bySeq = new Map<number, ChatMsg>();
  private readonly indexed = new Set<number>();

  /** 用现有消息建索引（调用方保证 seq 唯一；重复以最后一条为准）。 */
  static from(messages: ChatMsg[]): MessageIndex {
    const idx = new MessageIndex();
    idx.merge(messages);
    return idx;
  }

  get size(): number {
    return this.indexed.size;
  }

  /**
   * 合并一页消息（可新可旧），返回升序数组。
   *
   * 同 seq 视为同一条消息的新版本：内容以 `incoming` 为准，索引不重复插入。
   */
  merge(incoming: ChatMsg[]): ChatMsg[] {
    // 降序喂给 tweb 的切片（它按 slices[0]=最新、块内降序组织）
    const descending = [...incoming].sort((a, b) => b.seq - a.seq);
    const fresh: number[] = [];
    for (const m of descending) {
      this.bySeq.set(m.seq, m);
      if (!this.indexed.has(m.seq)) {
        this.indexed.add(m.seq);
        fresh.push(m.seq);
      }
    }
    if (fresh.length > 0) {
      this.index.insertSlice(fresh);
    }
    return this.toArray();
  }

  /** 升序（旧 → 新），与 `ChatMsg[]` 的既有约定一致。 */
  toArray(): ChatMsg[] {
    const out: ChatMsg[] = [];
    for (let i = this.index.slices.length - 1; i >= 0; i--) {
      const slice = this.index.slices[i];
      for (let j = slice.length - 1; j >= 0; j--) {
        const m = this.bySeq.get(slice[j]);
        if (m) out.push(m);
      }
    }
    return out;
  }

  /** 最近 n 条的 seq（窗口裁剪用；不复制消息对象）。 */
  tailSeqs(n: number): number[] {
    const out: number[] = [];
    for (const slice of this.index.slices) {
      for (let j = 0; j < slice.length && out.length < n; j++) {
        out.push(slice[j]);
      }
      if (out.length >= n) break;
    }
    return out;
  }
}

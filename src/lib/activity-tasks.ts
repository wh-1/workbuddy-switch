//! 活跃地图 / 成长任务 的并行状态查询（2026-09-30 自 AccountsPage 迁出：
//! 纯函数零状态耦合，放 lib 让页面文件相对上游保持最小插入）。

import * as api from "@/lib/api";
import type { ActivityStatus, TasksStatus } from "@/lib/types";

/** 并行查询各账号今日活跃地图状态；失败的账号不写入，由调用方保留原值。 */
export async function fetchActivityMap(
  accountIds: string[],
  isStale?: () => boolean,
): Promise<Record<string, ActivityStatus>> {
  const entries = await Promise.all(
    accountIds.map(async (id) => {
      try {
        const res = await api.getActivityStatus(id);
        if (isStale?.()) return null;
        return [id, res] as const;
      } catch {
        return null;
      }
    }),
  );
  const next: Record<string, ActivityStatus> = {};
  for (const entry of entries) {
    if (entry) next[entry[0]] = entry[1];
  }
  return next;
}

/** 并行查询各账号今日成长任务状态；失败的账号不写入，由调用方保留原值。 */
export async function fetchTasksMap(
  accountIds: string[],
  isStale?: () => boolean,
): Promise<Record<string, TasksStatus>> {
  const entries = await Promise.all(
    accountIds.map(async (id) => {
      try {
        const res = await api.getTasksStatus(id);
        if (isStale?.()) return null;
        return [id, res] as const;
      } catch {
        return null;
      }
    }),
  );
  const next: Record<string, TasksStatus> = {};
  for (const entry of entries) {
    if (entry) next[entry[0]] = entry[1];
  }
  return next;
}

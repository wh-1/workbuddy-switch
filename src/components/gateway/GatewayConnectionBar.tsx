// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { useEffect, useState } from "react";
import { Copy, PlugZap, RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { GatewayConfig, GatewayState } from "@/lib/gateway";
import { cn } from "@/lib/utils";

interface Props {
  config: GatewayConfig;
  state: GatewayState;
  error: string | null;
  serviceName?: string;
  onSave: (cfg: GatewayConfig) => void;
  onRefresh: () => void;
}

const STATE_BADGE: Record<GatewayState, { label: string; className: string }> = {
  idle: { label: "未连接", className: "bg-muted text-muted-foreground" },
  connecting: { label: "连接中…", className: "bg-amber-500/15 text-amber-600 dark:text-amber-400" },
  online: { label: "在线", className: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  offline: { label: "连不上", className: "bg-red-500/15 text-red-600 dark:text-red-400" },
};

export function GatewayConnectionBar({ config, state, error, serviceName, onSave, onRefresh }: Props) {
  const [baseUrl, setBaseUrl] = useState(config.baseUrl);
  const [apiKey, setApiKey] = useState(config.apiKey);
  const [saved, setSaved] = useState(false);

  // 外部配置变化（首次挂载）时同步草稿
  useEffect(() => {
    setBaseUrl(config.baseUrl);
    setApiKey(config.apiKey);
  }, [config]);

  const dirty = baseUrl !== config.baseUrl || apiKey !== config.apiKey;

  const handleSave = () => {
    onSave({ ...config, baseUrl: baseUrl.trim(), apiKey: apiKey.trim() });
    setSaved(true);
    setTimeout(() => setSaved(false), 1500);
  };

  const badge = STATE_BADGE[state];

  // 配置搬家：localStorage 里的网关配置（含 Key）在本机之间复制。
  const [ioOpen, setIoOpen] = useState(false);
  const [ioText, setIoText] = useState("");
  const [ioMsg, setIoMsg] = useState<string | null>(null);

  const handleExport = async () => {
    const payload = JSON.stringify({ baseUrl: config.baseUrl, apiKey: config.apiKey }, null, 2);
    try {
      await navigator.clipboard.writeText(payload);
      setIoMsg("已复制到剪贴板——粘到别处的「配置搬家」导入框就能用（内含 Key，别贴到外面）。");
    } catch {
      setIoText(payload);
      setIoMsg("剪贴板用不了，直接手动复制下面这段：");
    }
  };

  const handleImport = () => {
    try {
      const parsed = JSON.parse(ioText) as Partial<GatewayConfig>;
      if (typeof parsed.baseUrl !== "string" || !parsed.baseUrl.trim()) throw new Error("no baseUrl");
      setBaseUrl(parsed.baseUrl.trim());
      if (typeof parsed.apiKey === "string") setApiKey(parsed.apiKey);
      setIoMsg("已填入，点「保存并连接」生效。");
    } catch {
      setIoMsg("解析失败：要包含 baseUrl（和 apiKey）的 JSON。");
    }
  };

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder="网关地址，如 http://127.0.0.1:7863"
          className="h-9 w-64 font-mono text-xs"
        />
        <Input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder="API Key（只存本地，不会显示）"
          className="h-9 w-56 font-mono text-xs"
          autoComplete="off"
        />
        <Button size="sm" variant={dirty ? "default" : "secondary"} onClick={handleSave} disabled={!dirty}>
          {dirty ? "保存并连接" : "已保存"}
        </Button>
        <Button size="sm" variant="outline" onClick={onRefresh} title="重新拉取账号池状态">
          <RefreshCw className="size-4" />
          刷新
        </Button>
        <Button
          size="sm"
          variant="ghost"
          title="把网关配置复制到剪贴板，或从剪贴板粘回来"
          onClick={() => {
            setIoText("");
            setIoMsg(null);
            setIoOpen(true);
          }}
        >
          <Copy className="size-4" />
          配置搬家
        </Button>
        <Badge variant="outline" className={cn("gap-1", badge.className)}>
          <PlugZap className="size-3" />
          {badge.label}
          {state === "online" && serviceName ? ` · ${serviceName}` : ""}
        </Badge>
        {saved && !dirty && <span className="text-xs text-muted-foreground">配置已存到本机</span>}
      </div>
      {state === "offline" && error ? (
        <p className="text-xs text-muted-foreground">
          连不上：{error}。先确认 2api 网关起了没有，再核对地址和 Key；如果刚换过端口，记得同步改这里。
        </p>
      ) : null}

      <Dialog open={ioOpen} onOpenChange={setIoOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>配置搬家</DialogTitle>
            <DialogDescription>导出会把 API Key 一起带上（明文）——只在本机之间搬，别贴到外面。</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Button variant="outline" size="sm" onClick={() => void handleExport()}>
              <Copy className="size-4" />
              复制当前配置
            </Button>
            <Input
              value={ioText}
              onChange={(e) => setIoText(e.target.value)}
              placeholder={'粘贴配置 JSON，如 {"baseUrl":"http://127.0.0.1:7863","apiKey":"…"}'}
              className="font-mono text-xs"
              autoComplete="off"
            />
            <Button size="sm" variant="outline" disabled={!ioText.trim()} onClick={handleImport}>
              填入
            </Button>
            {ioMsg ? <p className="text-xs text-muted-foreground">{ioMsg}</p> : null}
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}

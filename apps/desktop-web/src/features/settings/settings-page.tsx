import { useState } from "react";
import { BellRing, Info, MonitorSmartphone, ShieldCheck, UserRound } from "lucide-react";
import { AccountSection } from "./account-section";
import { NotificationSection } from "./notification-section";
import { SecuritySection } from "./security-section";
import { PlatformSection } from "./platform-section";
import { AboutPage } from "./about-page";
import { PageHeader, cn } from "../../shell/ui";

const TABS = [
  { id: "account", label: "账号与外观", icon: UserRound },
  { id: "notification", label: "通知与同步", icon: BellRing },
  { id: "security", label: "安全与备份", icon: ShieldCheck },
  { id: "platform", label: "平台能力", icon: MonitorSmartphone },
  { id: "about", label: "关于", icon: Info },
] as const;

type TabId = (typeof TABS)[number]["id"];

export function SettingsPage() {
  const [tab, setTab] = useState<TabId>("account");
  const active = TABS.find((item) => item.id === tab);

  return (
    <div className="flex h-full min-h-0">
      <nav className="flex w-[156px] shrink-0 flex-col gap-0.5 border-r border-[var(--line)] bg-[var(--surface)] p-2" aria-label="设置分类">
        {TABS.map((item) => (
          <button
            key={item.id}
            type="button"
            aria-current={tab === item.id ? "page" : undefined}
            onClick={() => setTab(item.id)}
            className={cn(
              "flex items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[10.5px] transition",
              tab === item.id
                ? "bg-[var(--accent-soft)] font-medium text-[var(--accent-ink)]"
                : "text-[var(--ink-2)] hover:bg-[var(--surface-3)]",
            )}
            data-testid={`settings-tab-${item.id}`}
          >
            <item.icon size={13} /> {item.label}
          </button>
        ))}
        <p className="mt-auto px-2 py-1 text-[8.5px] leading-4 text-[var(--muted)]">
          设置通过 <code>preferences.set</code> 持久化；敏感能力单独走 <code>PlatformPort</code>。
        </p>
      </nav>

      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        {tab === "about" ? null : (
          <PageHeader title={active?.label ?? "设置"} description={tab === "platform" ? "真实的系统能力状态与测试入口。" : undefined} />
        )}
        <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {tab === "account" ? <AccountSection /> : null}
          {tab === "notification" ? <NotificationSection /> : null}
          {tab === "security" ? <SecuritySection /> : null}
          {tab === "platform" ? <PlatformSection /> : null}
          {tab === "about" ? <AboutPage embedded /> : null}
        </div>
      </div>
    </div>
  );
}

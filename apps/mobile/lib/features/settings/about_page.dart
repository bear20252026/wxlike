/// G13-05：帮助与关于。
///
/// 版本号来自构建参数（`--dart-define=APP_VERSION=...`），不在代码里写死。
/// 协议/帮助入口以 `entryId` 登记，由根组合根注入打开方式；未注入时明确显示
/// 阻塞原因，不弹"敬请期待"假提示。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';

class AgreementEntry {
  const AgreementEntry({required this.title, required this.entryId});

  final String title;

  /// 公共清单里的 entry_id；打开方式由 G00/G12 的平台桥提供。
  final String entryId;
}

/// 打开登记过的外部入口，返回是否真的打开成功。
typedef ExternalLinkOpener = Future<bool> Function(String entryId);

const List<AgreementEntry> g13Agreements = [
  AgreementEntry(title: '用户协议', entryId: 'settings.agreement.terms'),
  AgreementEntry(title: '隐私政策', entryId: 'settings.agreement.privacy'),
  AgreementEntry(title: '帮助中心', entryId: 'settings.help'),
];

const String g13AppVersion = String.fromEnvironment(
  'APP_VERSION',
  defaultValue: '开发构建',
);

class AboutPage extends StatelessWidget {
  const AboutPage({
    super.key,
    this.openExternal,
    this.agreements = g13Agreements,
    this.appVersion = g13AppVersion,
  });

  final ExternalLinkOpener? openExternal;
  final List<AgreementEntry> agreements;
  final String appVersion;

  Future<void> _open(BuildContext context, AgreementEntry entry) async {
    final opener = openExternal;
    if (opener == null) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('链接打开由平台桥提供，当前未接入（entry_id: ${entry.entryId}）')),
      );
      return;
    }
    try {
      final opened = await opener(entry.entryId);
      if (!context.mounted) return;
      if (!opened) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('链接未能打开，请稍后重试')));
      }
    } catch (error) {
      if (!context.mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('帮助与关于')),
      body: ListView(
        children: [
          const SizedBox(height: 24),
          Center(
            child: Column(
              children: [
                const CircleAvatar(
                  radius: 32,
                  child: Icon(Icons.chat_bubble_outline, size: 32),
                ),
                const SizedBox(height: 12),
                Text('wxlike', style: Theme.of(context).textTheme.titleMedium),
                Text(
                  '版本 $appVersion',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
                Text(
                  '界面契约 $g13ContractVersion',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ],
            ),
          ),
          const SizedBox(height: 24),
          const Divider(height: 1),
          for (final entry in agreements)
            ListTile(
              title: Text(entry.title),
              subtitle: Text(entry.entryId),
              trailing: const Icon(Icons.open_in_new, size: 18),
              onTap: () => _open(context, entry),
            ),
          const Divider(height: 1),
          const Padding(
            padding: EdgeInsets.all(16),
            child: Text(
              '本应用以 AGPL-3.0-only 发布。第三方组件与参考实现的许可边界见仓库 REFERENCES。',
              style: TextStyle(fontSize: 12),
            ),
          ),
        ],
      ),
    );
  }
}

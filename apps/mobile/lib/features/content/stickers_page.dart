/// G13-01：表情包。
///
/// 下载是写操作：按钮在请求期间禁用，成功后以服务端返回的包状态刷新，
/// 不先本地改成"已下载"再等同步。
library;

import 'package:flutter/material.dart';

import 'g13/core.dart';
import 'g13/models.dart';
import 'g13/ports.dart';
import 'g13/widgets.dart';

class StickersPage extends StatefulWidget {
  const StickersPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<StickersPage> createState() => _StickersPageState();
}

class _StickersPageState extends State<StickersPage> {
  late Future<PageResult<StickerPack>> _future = widget.ports.content
      .stickerPacks();
  final Set<String> _busy = <String>{};

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.content.stickerPacks();
    });
  }

  Future<void> _download(StickerPack pack) async {
    if (!_busy.add(pack.id)) return;
    setState(() {});
    try {
      await widget.ports.content.downloadStickerPack(
        pack.id,
        idempotencyKey: 'sticker-${pack.id}-${pack.count}',
      );
      _reload();
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    } finally {
      if (mounted) setState(() => _busy.remove(pack.id));
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '表情',
      onBack: () => Navigator.of(context).maybePop(),
      body: AsyncView<PageResult<StickerPack>>(
        future: _future,
        onRetry: _reload,
        isEmpty: (page) => page.isEmpty,
        emptyText: '没有可用的表情包',
        builder: (context, page) => GridView.builder(
          padding: const EdgeInsets.all(12),
          gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
            crossAxisCount: 2,
            mainAxisSpacing: 12,
            crossAxisSpacing: 12,
            childAspectRatio: 0.95,
          ),
          itemCount: page.items.length,
          itemBuilder: (context, index) {
            final pack = page.items[index];
            return Card(
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: pack.downloaded ? null : () => _download(pack),
                child: Padding(
                  padding: const EdgeInsets.all(12),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: Center(
                          child: Text(
                            pack.cover,
                            style: const TextStyle(fontSize: 40),
                          ),
                        ),
                      ),
                      Text(
                        pack.name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      const SizedBox(height: 4),
                      Row(
                        children: [
                          Expanded(child: Text('${pack.count} 个')),
                          if (_busy.contains(pack.id))
                            const SizedBox(
                              width: 16,
                              height: 16,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          else if (pack.downloaded)
                            const Pill(text: '已下载')
                          else
                            const Pill(text: '下载'),
                        ],
                      ),
                    ],
                  ),
                ),
              ),
            );
          },
        ),
      ),
    );
  }
}

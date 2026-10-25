/// G13-04：小程序目录与最近使用。
///
/// 列表、安装、授权范围与启动流程都来自 `mini.*`；启动返回的是作业句柄，
/// 真正的容器由 G08 提供。已撤销/未安装的条目不能"点开看看"——那会让人
/// 以为宿主已经可用。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class MiniProgramsPage extends StatefulWidget {
  const MiniProgramsPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<MiniProgramsPage> createState() => _MiniProgramsPageState();
}

class _MiniProgramsPageState extends State<MiniProgramsPage> {
  String _tab = 'recent';
  late Future<PageResult<MiniAppInfo>> _future = _load();
  final Set<String> _busy = <String>{};

  Future<PageResult<MiniAppInfo>> _load() => _tab == 'recent'
      ? widget.ports.mini.recent()
      : widget.ports.mini.catalog();

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = _load();
    });
  }

  void _report(Object error) {
    if (!mounted) return;
    final text = error is G13Failure ? error.displayText : '$error';
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _install(MiniAppInfo app) async {
    if (!_busy.add(app.appId)) return;
    setState(() {});
    try {
      await widget.ports.mini.install(
        app.appId,
        idempotencyKey: 'mini-install-${app.appId}-${app.version}',
      );
      _reload();
    } catch (error) {
      _report(error);
    } finally {
      if (mounted) setState(() => _busy.remove(app.appId));
    }
  }

  Future<void> _launch(MiniAppInfo app) async {
    if (_busy.contains(app.appId)) return;
    setState(() => _busy.add(app.appId));
    try {
      final operation = await widget.ports.mini.launch(
        app.appId,
        idempotencyKey: 'mini-launch-${app.appId}-${app.version}',
      );
      if (!mounted) return;
      await showModalBottomSheet<void>(
        context: context,
        showDragHandle: true,
        builder: (context) => _OperationSheet(
          ports: widget.ports,
          initial: operation,
          title: app.name,
        ),
      );
    } catch (error) {
      _report(error);
    } finally {
      if (mounted) setState(() => _busy.remove(app.appId));
    }
  }

  Future<void> _showScopes(MiniAppInfo app) async {
    try {
      final scopes = await widget.ports.mini.grantedScopes(app.appId);
      if (!mounted) return;
      showModalBottomSheet<void>(
        context: context,
        showDragHandle: true,
        builder: (context) => SafeArea(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(app.name, style: Theme.of(context).textTheme.titleMedium),
                const SizedBox(height: 8),
                Text('版本 ${app.version}'),
                const SizedBox(height: 8),
                Text('已授权范围：${scopes.isEmpty ? '无' : scopes.join('、')}'),
              ],
            ),
          ),
        ),
      );
    } catch (error) {
      _report(error);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '小程序',
      onBack: () => Navigator.of(context).maybePop(),
      body: Column(
        children: [
          Padding(
            padding: const EdgeInsets.all(12),
            child: SegmentedButton<String>(
              segments: const [
                ButtonSegment(value: 'recent', label: Text('最近使用')),
                ButtonSegment(value: 'catalog', label: Text('全部')),
              ],
              selected: {_tab},
              onSelectionChanged: (value) {
                setState(() {
                  _tab = value.first;
                  _future = _load();
                });
              },
            ),
          ),
          Expanded(
            child: AsyncView<PageResult<MiniAppInfo>>(
              future: _future,
              onRetry: _reload,
              isEmpty: (page) => page.isEmpty,
              emptyText: _tab == 'recent' ? '还没有使用过小程序' : '目录为空',
              builder: (context, page) => ListView.separated(
                itemCount: page.items.length,
                separatorBuilder: (_, _) => const Divider(height: 1),
                itemBuilder: (context, index) {
                  final app = page.items[index];
                  return ListTile(
                    leading: AvatarDot(
                      label: app.name,
                      color: const Color(0xff07b25a),
                    ),
                    title: Text(app.name),
                    subtitle: Text(
                      app.revoked
                          ? '版本已撤销，无法启动'
                          : 'v${app.version}${app.installed ? ' · 已安装' : ''}',
                    ),
                    trailing: _busy.contains(app.appId)
                        ? const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : Wrap(
                            spacing: 4,
                            children: [
                              if (!app.installed && !app.revoked)
                                TextButton(
                                  onPressed: () => _install(app),
                                  child: const Text('安装'),
                                ),
                              if (app.installed && !app.revoked)
                                TextButton(
                                  onPressed: () => _launch(app),
                                  child: const Text('打开'),
                                ),
                              IconButton(
                                tooltip: '授权范围',
                                onPressed: () => _showScopes(app),
                                icon: const Icon(
                                  Icons.shield_outlined,
                                  size: 18,
                                ),
                              ),
                            ],
                          ),
                    onTap: app.revoked ? null : () => _showScopes(app),
                  );
                },
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _OperationSheet extends StatefulWidget {
  const _OperationSheet({
    required this.ports,
    required this.initial,
    required this.title,
  });

  final G13Ports ports;
  final OperationHandle initial;
  final String title;

  @override
  State<_OperationSheet> createState() => _OperationSheetState();
}

class _OperationSheetState extends State<_OperationSheet> {
  late OperationHandle _operation = widget.initial;
  bool _busy = false;
  Object? _error;

  Future<void> _refresh() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final operation = await widget.ports.mini.operation(
        _operation.operationId,
      );
      if (mounted) setState(() => _operation = operation);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(widget.title, style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 8),
            Text('状态：${_operation.state}'),
            const SizedBox(height: 4),
            LinearProgressIndicator(value: _operation.progress.clamp(0, 1)),
            const SizedBox(height: 8),
            Text(
              _operation.isSucceeded
                  ? '容器已就绪（由 G08 宿主渲染）'
                  : '启动尚未完成；界面不会用静态页冒充已打开',
              style: Theme.of(context).textTheme.bodySmall,
            ),
            if (_error != null) ...[
              const SizedBox(height: 8),
              InlineFailure(error: _error!),
            ],
            const SizedBox(height: 12),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  onPressed: () => Navigator.of(context).pop(),
                  child: const Text('关闭'),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: _busy ? null : _refresh,
                  child: const Text('刷新状态'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

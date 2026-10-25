/// G13-04：附近与摇一摇。
///
/// 只使用粗粒度网格（`cell`），不向界面下发精确坐标；位置授权被撤回后
/// `cellProvider` 返回 null，页面立即停止查询并说明原因。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

/// 返回当前粗粒度网格；无授权/定位关闭时返回 null。
typedef CellProvider = Future<String?> Function();

class NearbyPage extends StatefulWidget {
  const NearbyPage({
    super.key,
    required this.ports,
    this.shake = false,
    this.cellProvider,
  });

  final G13Ports ports;

  /// 从"摇一摇"进入时先给一次匹配动作。
  final bool shake;
  final CellProvider? cellProvider;

  @override
  State<NearbyPage> createState() => _NearbyPageState();
}

class _NearbyPageState extends State<NearbyPage> {
  Future<PageResult<NearbyPlace>>? _future;
  String? _cell;
  Object? _error;
  bool _locating = false;

  @override
  void initState() {
    super.initState();
    _locate();
  }

  Future<void> _locate() async {
    final provider = widget.cellProvider;
    if (provider == null) {
      setState(() {
        _error = const G13Failure(
          G13FailureKind.unavailable,
          'LOCATION_NOT_WIRED',
          '定位由平台桥提供，当前未接入',
        );
      });
      return;
    }
    setState(() {
      _locating = true;
      _error = null;
    });
    try {
      final cell = await provider();
      if (!mounted) return;
      if (cell == null || cell.isEmpty) {
        setState(() {
          _error = const G13Failure(
            G13FailureKind.forbidden,
            'LOCATION_DENIED',
            '定位权限未开启，撤回授权后不再查询附近',
          );
        });
        return;
      }
      setState(() {
        _cell = cell;
        _future = widget.ports.discovery.nearby(cell: cell);
      });
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _locating = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final future = _future;
    return G13Scaffold(
      title: widget.shake ? '摇一摇' : '附近',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(
          onPressed: _locating ? null : _locate,
          icon: const Icon(Icons.refresh),
        ),
      ],
      body: Column(
        children: [
          if (_error != null) InlineFailure(error: _error!, onRetry: _locate),
          if (widget.shake)
            Padding(
              padding: const EdgeInsets.all(12),
              child: OutlinedButton.icon(
                onPressed: _locating ? null : _locate,
                icon: const Icon(Icons.vibration),
                label: const Text('再摇一次'),
              ),
            ),
          Expanded(
            child: future == null
                ? EmptyView(
                    text: _error == null ? '正在获取粗粒度位置…' : '未开始查询',
                    hint: _cell == null ? '' : '当前网格：$_cell',
                  )
                : AsyncView<PageResult<NearbyPlace>>(
                    future: future,
                    onRetry: _locate,
                    isEmpty: (page) => page.isEmpty,
                    emptyText: '附近没有可展示的内容',
                    emptyHint: _cell == null ? '' : '当前网格：$_cell',
                    builder: (context, page) => ListView.separated(
                      itemCount: page.items.length,
                      separatorBuilder: (_, _) => const Divider(height: 1),
                      itemBuilder: (context, index) {
                        final place = page.items[index];
                        return ListTile(
                          leading: const Icon(Icons.place_outlined),
                          title: Text(place.name),
                          subtitle: Text(
                            '${place.distanceMeters} m · 网格 ${place.cell}',
                          ),
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

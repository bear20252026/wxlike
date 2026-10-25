/// G13-06：位置消息与实时位置共享。
///
/// 只对调用方传入的会话生效（`convId`），发送成功即返回调用方页面，不存在
/// "固定示例会话"。地图/导航由平台桥渲染（G12-05），未接入时显示明确的阻塞
/// 占位，不拿 SVG 假装真实地点。本页不持有定位订阅，离页即停止查询。
library;

import 'package:flutter/material.dart';

import '../../content/g13/core.dart';
import '../../content/g13/models.dart';
import '../../content/g13/ports.dart';
import '../../content/g13/widgets.dart';

/// 渲染真实地图；坐标来自服务端返回的位置。
typedef MapPreviewBuilder = Widget Function(
  BuildContext context,
  double latitude,
  double longitude,
);

/// 调起系统导航；返回是否真正打开。
typedef NavigationLauncher = Future<bool> Function(
  double latitude,
  double longitude,
  String placeName,
);

class LocationPage extends StatefulWidget {
  const LocationPage({
    super.key,
    required this.ports,
    required this.convId,
    this.mapBuilder,
    this.navigate,
  });

  final G13Ports ports;
  final String convId;
  final MapPreviewBuilder? mapBuilder;
  final NavigationLauncher? navigate;

  @override
  State<LocationPage> createState() => _LocationPageState();
}

class _LocationPageState extends State<LocationPage> {
  final TextEditingController _query = TextEditingController();
  late Future<PageResult<NearbyPlace>> _places = widget.ports.location.places(
    '',
  );

  LocationShare? _current;
  List<LocationShare> _shares = const [];
  bool _loadingShares = true;
  Object? _sharesError;

  String _shareKey = '';
  int _shareMinutes = 0;
  Object? _shareError;
  bool _startingShare = false;

  @override
  void initState() {
    super.initState();
    _loadShares();
  }

  @override
  void dispose() {
    // 页面不订阅定位流；关闭时无需额外释放，共享会话由用户显式停止。
    _query.dispose();
    super.dispose();
  }

  Future<void> _loadShares() async {
    setState(() {
      _loadingShares = true;
      _sharesError = null;
    });
    try {
      final current = await widget.ports.location.currentPlace(widget.convId);
      final active = await widget.ports.location.activeShares(widget.convId);
      if (!mounted) return;
      setState(() {
        _current = current;
        _shares = active;
      });
    } catch (error) {
      if (mounted) setState(() => _sharesError = error);
    } finally {
      if (mounted) setState(() => _loadingShares = false);
    }
  }

  void _search(String value) {
    setState(() {
      _places = widget.ports.location.places(value.trim());
    });
  }

  Future<void> _sendPlace(NearbyPlace place) async {
    final sent = await showModalBottomSheet<LocationShare>(
      context: context,
      showDragHandle: true,
      builder: (context) => _SendPlaceSheet(
        ports: widget.ports,
        convId: widget.convId,
        place: place,
      ),
    );
    if (sent == null || !mounted) return;
    Navigator.of(context).maybePop(sent);
  }

  Future<void> _startSharing() async {
    final chosen = await showDialog<int>(
      context: context,
      builder: (context) => SimpleDialog(
        title: const Text('共享实时位置'),
        children: [
          for (final minutes in const [5, 15, 60])
            ListTile(
              title: Text('$minutes 分钟'),
              onTap: () => Navigator.of(context).pop(minutes),
            ),
        ],
      ),
    );
    if (chosen == null || !mounted) return;
    _shareMinutes = chosen;
    _shareKey = newIdempotencyKey();
    await _shareWithKey();
  }

  Future<void> _shareWithKey() async {
    if (_startingShare || _shareMinutes <= 0 || _shareKey.isEmpty) return;
    setState(() {
      _startingShare = true;
      _shareError = null;
    });
    try {
      final share = await widget.ports.location.startSharing(
        widget.convId,
        minutes: _shareMinutes,
        idempotencyKey: _shareKey,
      );
      if (!mounted) return;
      setState(() {
        _current = share;
        _shares = [..._shares.where((item) => item.id != share.id), share];
        _shareKey = '';
      });
    } catch (error) {
      if (mounted) setState(() => _shareError = error);
    } finally {
      if (mounted) setState(() => _startingShare = false);
    }
  }

  Future<void> _stopSharing(LocationShare share) async {
    try {
      await widget.ports.location.stopSharing(
        share.id,
        idempotencyKey: 'stop-share-${share.id}',
      );
      if (!mounted) return;
      setState(() {
        _shares = _shares.where((item) => item.id != share.id).toList();
        if (_current?.id == share.id) _current = null;
        _sharesError = null;
      });
    } catch (error) {
      if (mounted) setState(() => _sharesError = error);
    }
  }

  Future<void> _navigate(LocationShare share) async {
    final launcher = widget.navigate;
    if (launcher == null) {
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text('导航由平台桥提供，当前未接入')));
      return;
    }
    try {
      final opened = await launcher(
        share.latitude,
        share.longitude,
        share.placeName,
      );
      if (!mounted) return;
      if (!opened) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('未能打开系统导航')));
      }
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  @override
  Widget build(BuildContext context) {
    final current = _current;
    return G13Scaffold(
      title: '位置',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(
          onPressed: _loadingShares ? null : _loadShares,
          icon: const Icon(Icons.refresh),
        ),
      ],
      body: ListView(
        children: [
          _mapArea(current),
          if (_sharesError != null)
            InlineFailure(
              error: _sharesError!,
              onRetry:
                  _sharesError is G13Failure &&
                      (_sharesError! as G13Failure).retryable
                  ? _loadShares
                  : null,
            ),
          if (_shareError != null)
            InlineFailure(
              error: _shareError!,
              onRetry:
                  _shareError is G13Failure &&
                      (_shareError! as G13Failure).retryable
                  ? _shareWithKey
                  : null,
            ),
          if (current != null) ...[
            const SectionTitle(text: '当前共享位置'),
            Card(
              margin: const EdgeInsets.symmetric(horizontal: 16),
              child: ListTile(
                leading: const Icon(Icons.location_on),
                title: Text(current.placeName),
                subtitle: Text(
                  '${current.address}\n共享至 ${formatTime(current.expiresAt)}',
                ),
                isThreeLine: true,
                trailing: TextButton(
                  onPressed: () => _stopSharing(current),
                  child: const Text('停止'),
                ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
              child: Row(
                children: [
                  OutlinedButton.icon(
                    onPressed: () => _navigate(current),
                    icon: const Icon(Icons.navigation_outlined),
                    label: const Text('导航'),
                  ),
                  const SizedBox(width: 12),
                  FilledButton.tonal(
                    onPressed: _startingShare ? null : _startSharing,
                    child: const Text('更新共享'),
                  ),
                ],
              ),
            ),
          ] else
            Padding(
              padding: const EdgeInsets.all(16),
              child: FilledButton.tonalIcon(
                onPressed: _startingShare ? null : _startSharing,
                icon: const Icon(Icons.share_location),
                label: const Text('共享实时位置'),
              ),
            ),
          if (_shares.length > 1) ...[
            const SectionTitle(text: '进行中的共享'),
            for (final share in _shares)
              if (share.id != current?.id)
                ListTile(
                  leading: const Icon(Icons.share_location_outlined),
                  title: Text(share.placeName),
                  subtitle: Text('到期 ${formatTime(share.expiresAt)}'),
                  trailing: TextButton(
                    onPressed: () => _stopSharing(share),
                    child: const Text('停止'),
                  ),
                ),
          ],
          const Divider(height: 32),
          const SectionTitle(text: '发送位置'),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: TextField(
              controller: _query,
              textInputAction: TextInputAction.search,
              onSubmitted: _search,
              decoration: const InputDecoration(
                hintText: '搜索地点',
                prefixIcon: Icon(Icons.search),
                border: OutlineInputBorder(),
                isDense: true,
              ),
            ),
          ),
          const SizedBox(height: 8),
          SizedBox(
            height: 320,
            child: AsyncView<PageResult<NearbyPlace>>(
              future: _places,
              onRetry: () => _search(_query.text),
              isEmpty: (page) => page.isEmpty,
              emptyText: '没有找到地点',
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
                    onTap: () => _sendPlace(place),
                  );
                },
              ),
            ),
          ),
          const SizedBox(height: 24),
        ],
      ),
    );
  }

  Widget _mapArea(LocationShare? current) {
    if (current == null) {
      return const Padding(
        padding: EdgeInsets.all(16),
        child: EmptyView(text: '还没有共享位置', hint: '开始共享后，服务端下发的位置会显示在地图上'),
      );
    }
    final builder = widget.mapBuilder;
    if (builder == null) {
      return Container(
        height: 180,
        margin: const EdgeInsets.all(16),
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: Theme.of(context).colorScheme.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(12),
        ),
        child: const Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.map_outlined, size: 40),
            SizedBox(height: 8),
            Text('地图渲染由平台桥提供，当前未接入（G12-05）'),
            SizedBox(height: 4),
            Text('坐标已就绪，接入后在此显示真实地图', style: TextStyle(fontSize: 12)),
          ],
        ),
      );
    }
    return Padding(
      padding: const EdgeInsets.all(16),
      child: SizedBox(
        height: 180,
        child: builder(context, current.latitude, current.longitude),
      ),
    );
  }
}

class _SendPlaceSheet extends StatefulWidget {
  const _SendPlaceSheet({
    required this.ports,
    required this.convId,
    required this.place,
  });

  final G13Ports ports;
  final String convId;
  final NearbyPlace place;

  @override
  State<_SendPlaceSheet> createState() => _SendPlaceSheetState();
}

class _SendPlaceSheetState extends State<_SendPlaceSheet> {
  final String _key = newIdempotencyKey();
  bool _sending = false;
  Object? _error;

  Future<void> _send() async {
    if (_sending) return;
    setState(() {
      _sending = true;
      _error = null;
    });
    try {
      final share = await widget.ports.location.sendLocation(
        widget.convId,
        widget.place.id,
        idempotencyKey: _key,
      );
      if (!mounted) return;
      Navigator.of(context).pop(share);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _sending = false);
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
            Text('发送位置', style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 12),
            ListTile(
              contentPadding: EdgeInsets.zero,
              leading: const Icon(Icons.place),
              title: Text(widget.place.name),
              subtitle: Text(
                '${widget.place.distanceMeters} m · 网格 ${widget.place.cell}',
              ),
            ),
            if (_error != null) InlineFailure(error: _error!),
            const SizedBox(height: 8),
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton(
                onPressed: _sending ? null : _send,
                child: _sending
                    ? const SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Text('发送到当前会话'),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

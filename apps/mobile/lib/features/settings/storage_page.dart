/// G13-05：存储空间与备份导出。
///
/// 大小来自 `profile.storage`，清缓存后重新读取服务端结果；备份导出是长操作，
/// 只展示服务端作业状态，查询结果必须校验 `verified` 后才算可用。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class StoragePage extends StatefulWidget {
  const StoragePage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<StoragePage> createState() => _StoragePageState();
}

class _StoragePageState extends State<StoragePage> {
  StorageUsage? _usage;
  Object? _error;
  bool _loading = true;
  bool _clearing = false;
  String _clearKey = newIdempotencyKey();

  OperationHandle? _export;
  BackupFile? _backup;
  Object? _backupError;
  bool _exporting = false;
  final String _exportKey = newIdempotencyKey();

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final usage = await widget.ports.profile.storage();
      if (!mounted) return;
      setState(() => _usage = usage);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  Future<void> _clearCache() async {
    if (_clearing) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('清理缓存'),
        content: const Text('将清理可再生成的缓存文件，不会删除聊天记录与收藏。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('清理'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() {
      _clearing = true;
      _error = null;
    });
    try {
      final updated = await widget.ports.profile.clearCache(
        idempotencyKey: _clearKey,
      );
      if (!mounted) return;
      setState(() {
        _usage = updated;
        _clearKey = newIdempotencyKey();
      });
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _clearing = false);
    }
  }

  Future<void> _exportBackup() async {
    if (_exporting) return;
    setState(() {
      _exporting = true;
      _backupError = null;
    });
    try {
      final handle = await widget.ports.profile.exportBackup(
        idempotencyKey: _exportKey,
      );
      if (!mounted) return;
      setState(() => _export = handle);
    } catch (error) {
      if (mounted) setState(() => _backupError = error);
    } finally {
      if (mounted) setState(() => _exporting = false);
    }
  }

  Future<void> _fetchBackup() async {
    final export = _export;
    if (export == null || _exporting) return;
    setState(() {
      _exporting = true;
      _backupError = null;
    });
    try {
      final file = await widget.ports.profile.backupResult(export.operationId);
      if (!mounted) return;
      setState(() => _backup = file);
    } catch (error) {
      if (mounted) setState(() => _backupError = error);
    } finally {
      if (mounted) setState(() => _exporting = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final usage = _usage;
    if (usage == null) {
      if (_loading) {
        return G13Scaffold(
          title: '存储空间',
          onBack: () => Navigator.of(context).maybePop(),
          body: const Center(child: CircularProgressIndicator()),
        );
      }
      return G13Scaffold(
        title: '存储空间',
        onBack: () => Navigator.of(context).maybePop(),
        body: FailureView(error: _error ?? '存储信息不可用', onRetry: _load),
      );
    }
    final export = _export;
    final backup = _backup;
    return G13Scaffold(
      title: '存储空间',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(
          onPressed: _loading ? null : _load,
          icon: const Icon(Icons.refresh),
        ),
      ],
      body: ListView(
        children: [
          if (_error != null)
            InlineFailure(
              error: _error!,
              onRetry: _error is G13Failure && (_error! as G13Failure).retryable
                  ? _load
                  : null,
            ),
          const SectionTitle(text: '占用'),
          ListTile(
            leading: const Icon(Icons.storage_outlined),
            title: const Text('总占用'),
            trailing: Text(formatBytes(usage.totalBytes)),
          ),
          ListTile(
            title: const Text('缓存'),
            trailing: Text(formatBytes(usage.cacheBytes)),
          ),
          ListTile(
            title: const Text('媒体'),
            trailing: Text(formatBytes(usage.mediaBytes)),
          ),
          ListTile(
            title: const Text('数据库'),
            trailing: Text(formatBytes(usage.databaseBytes)),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: FilledButton.tonal(
              onPressed: _clearing ? null : _clearCache,
              child: _clearing
                  ? const SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Text('清理缓存'),
            ),
          ),
          const Divider(height: 32),
          const SectionTitle(text: '聊天备份'),
          const Padding(
            padding: EdgeInsets.symmetric(horizontal: 16),
            child: Text(
              '导出由服务端生成加密备份文件；下载完成后需校验摘要才能用于恢复。',
              style: TextStyle(fontSize: 12),
            ),
          ),
          if (_backupError != null)
            InlineFailure(
              error: _backupError!,
              onRetry:
                  _backupError is G13Failure &&
                      (_backupError! as G13Failure).retryable
                  ? _fetchBackup
                  : null,
            ),
          Padding(
            padding: const EdgeInsets.all(16),
            child: Row(
              children: [
                FilledButton.tonal(
                  onPressed: _exporting ? null : _exportBackup,
                  child: const Text('导出备份'),
                ),
                const SizedBox(width: 12),
                if (export != null)
                  TextButton(
                    onPressed: _exporting ? null : _fetchBackup,
                    child: const Text('查询备份文件'),
                  ),
              ],
            ),
          ),
          if (export != null) ...[
            ListTile(
              title: const Text('导出作业'),
              subtitle: Text(
                '${export.state} · ${(export.progress * 100).toStringAsFixed(0)}%',
              ),
              trailing: Text(export.operationId),
            ),
            LinearProgressIndicator(value: export.progress.clamp(0, 1)),
            if (export.isTerminal && !export.isSucceeded)
              const Padding(
                padding: EdgeInsets.all(16),
                child: Text('导出未成功，请重新发起。'),
              ),
          ],
          if (backup != null) ...[
            ListTile(
              title: Text(backup.verified ? '备份文件已校验' : '备份文件未通过校验'),
              subtitle: Text(
                '${formatTime(backup.createdAt)} · ${formatBytes(backup.sizeBytes)}',
              ),
              trailing: backup.verified
                  ? const Pill(text: '可恢复')
                  : const Pill(text: '不可用'),
            ),
          ],
          const SizedBox(height: 24),
        ],
      ),
    );
  }
}

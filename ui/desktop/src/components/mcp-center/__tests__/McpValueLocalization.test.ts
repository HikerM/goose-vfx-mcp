import { createIntl, createIntlCache } from 'react-intl';
import { describe, expect, it } from 'vitest';
import de from '../../../i18n/messages/de.json';
import es from '../../../i18n/messages/es.json';
import fr from '../../../i18n/messages/fr.json';
import hi from '../../../i18n/messages/hi.json';
import id from '../../../i18n/messages/id.json';
import itCatalog from '../../../i18n/messages/it.json';
import ja from '../../../i18n/messages/ja.json';
import ko from '../../../i18n/messages/ko.json';
import ms from '../../../i18n/messages/ms.json';
import pt from '../../../i18n/messages/pt.json';
import ru from '../../../i18n/messages/ru.json';
import tr from '../../../i18n/messages/tr.json';
import vi from '../../../i18n/messages/vi.json';
import zhCN from '../../../i18n/messages/zh-CN.json';
import zhTW from '../../../i18n/messages/zh-TW.json';
import { formatMcpValue } from '../McpCenterCommon';

type MessageCatalog = Record<string, { defaultMessage: string }>;

function messages(catalog: MessageCatalog): Record<string, string> {
  return Object.fromEntries(
    Object.entries(catalog).map(([id, message]) => [id, message.defaultMessage])
  );
}

describe('MCP enum localization', () => {
  it.each([
    ['zh-CN', zhCN, 'filesystem_write', '写入文件'],
    ['zh-TW', zhTW, 'recovery_required', '需要復原'],
    ['ja', ja, 'blocked_auth', '認証でブロック'],
    ['es', es, 'streamable_http', 'HTTP transmisible'],
    ['de', de, 'policy_denied', 'Durch Richtlinie abgelehnt'],
    ['fr', fr, 'available_after_materialization', 'Disponible après matérialisation'],
  ] as const)(
    'renders %s values without exposing the raw wire value',
    (locale, catalog, wire, label) => {
      const intl = createIntl(
        { locale, messages: messages(catalog as MessageCatalog) },
        createIntlCache()
      );

      expect(formatMcpValue(intl, wire)).toBe(label);
      expect(formatMcpValue(intl, wire)).not.toBe(wire);
    }
  );

  it.each([
    ['de', de, 'Unbekannter oder nicht unterstützter Status'],
    ['es', es, 'Estado desconocido o no compatible'],
    ['fr', fr, 'État inconnu ou non pris en charge'],
    ['hi', hi, 'अज्ञात या असमर्थित स्थिति'],
    ['id', id, 'Status tidak dikenal atau tidak didukung'],
    ['it', itCatalog, 'Stato sconosciuto o non supportato'],
    ['ja', ja, '不明または未対応の状態'],
    ['ko', ko, '알 수 없거나 지원되지 않는 상태'],
    ['ms', ms, 'Status tidak diketahui atau tidak disokong'],
    ['pt', pt, 'Estado desconhecido ou não suportado'],
    ['ru', ru, 'Неизвестное или неподдерживаемое состояние'],
    ['tr', tr, 'Bilinmeyen veya desteklenmeyen durum'],
    ['vi', vi, 'Trạng thái không xác định hoặc không được hỗ trợ'],
    ['zh-CN', zhCN, '未知或不支持的状态'],
    ['zh-TW', zhTW, '未知或不支援的狀態'],
  ] as const)('renders unknown wire values as a safe %s fallback', (locale, catalog, fallback) => {
    const intl = createIntl(
      { locale, messages: messages(catalog as MessageCatalog) },
      createIntlCache()
    );

    expect(formatMcpValue(intl, 'future_wire_value')).toBe(fallback);
    expect(formatMcpValue(intl, 'future_wire_value')).not.toContain('future_wire_value');
  });
});

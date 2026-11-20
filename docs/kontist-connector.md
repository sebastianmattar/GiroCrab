# Kontist-Anbindung

<!-- sync:connector-note:start -->
> **Hinweis / Note:** Dieser Connector ist ein unabhängiges Community-Projekt und steht in keiner Verbindung zur Kontist. Schnittstelle: offiziell. Bei inoffiziellen Schnittstellen kann die Nutzung gegen die Nutzungsbedingungen der Bank verstoßen.
> This connector is an independent community project and is not affiliated with Kontist. Interface: official. Using unofficial interfaces may violate the bank's terms of service.
<!-- sync:connector-note:end -->

Status: **Verifiziert** – `ConnectorKind::Kontist` (Schlüssel `kontist`), offizielle GraphQL-API von Kontist in `girocrab-core/src/connectors/kontist.rs`, kein Browserfenster.

Kontist veröffentlicht die API unter [kontist.dev](https://kontist.dev/). GiroCrab liest das Hauptkonto (Saldo, IBAN, Inhaber) und dessen Umsätze; Überweisungen, Belege und Steuerfunktionen nutzt der Connector nicht.

## Einrichtung

Kontist lässt Programme nur über einen eigenen API-Client an das Konto. Diesen Client legt jede Nutzerin und jeder Nutzer einmal selbst an; GiroCrab bringt keinen mit.

1. Auf [kontist.dev](https://kontist.dev/) mit den normalen Kontist-Zugangsdaten anmelden und **Client Management** öffnen.
2. Einen neuen Client anlegen:
   * **Name:** frei wählbar, z. B. `GiroCrab`
   * **Redirect URI:** ist ein Pflichtfeld, wird von GiroCrab aber nicht benutzt. `https://localhost/girocrab/callback` eintragen.
   * **Scopes:** **Offline**, **Accounts**, **Transactions** und **Users** anhaken. Transfers, Subscriptions und Statements braucht GiroCrab nicht.
3. kontist.dev zeigt danach die **Client ID** des neuen Clients an (eine lange Kennung der Form `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`). Grant-Typen lassen sich dort nicht auswählen, und meist wird kein Secret angezeigt; beides ist in Ordnung.
4. In GiroCrab unter Einstellungen › Kontist die **Client-ID** eintragen. Das Feld **Client-Secret** bleibt leer, außer kontist.dev hat ausdrücklich ein Secret angezeigt.
5. Bankzugang „Kontist“ anlegen: **E-Mail-Adresse** und **Passwort** des Kontist-Kontos.
6. Den ersten Abruf starten und die Anmeldung in der **Kontist-App** bestätigen. Weitere Abrufe laufen ohne Bestätigung, solange Kontist die Anmeldung gelten lässt.

Die Client-ID ist kein Passwort, gibt aber zusammen mit E-Mail-Adresse und Passwort Zugriff auf das Konto. Einen Client, den man nicht mehr nutzt, löscht man auf kontist.dev wieder.

### Fehlermeldungen beim Einrichten

| Meldung | Ursache und Abhilfe |
|---|---|
| „Die OAuth2-Client-ID (oder das Secret) wurde abgelehnt“ | Client-ID falsch kopiert, oder ein Secret eingetragen, obwohl der Client keines hat. Client-ID auf kontist.dev vergleichen, Secret-Feld leeren. |
| „Dem OAuth2-Client fehlen Berechtigungen“ | Beim Client fehlt einer der Scopes Offline, Accounts, Transactions oder Users. Auf kontist.dev nachtragen. |
| „Anmeldung fehlgeschlagen. Bitte E-Mail-Adresse und Passwort prüfen.“ | E-Mail-Adresse oder Passwort des Kontist-Kontos stimmen nicht. |
| „Die Freigabe ist abgelaufen“ | Die Bestätigung in der Kontist-App kam nicht rechtzeitig. Abruf neu starten. |

## Ablauf

| Schritt | Anfrage | Auswertung |
|---|---|---|
| 1 | `POST /api/oauth/token` mit `grant_type=refresh_token` | Nur wenn ein Refresh-Token aus einem früheren Abruf gespeichert ist. Klappt das, entfallen die Schritte 2–4. |
| 2 | `POST /api/oauth/token` mit `grant_type=password` | E-Mail-Adresse, Passwort und Scopes; das Client-Secret geht als HTTP-Basic-Auth, ein Client ohne Secret schickt nur `client_id`. |
| 3 | `POST /api/user/mfa/challenges` | Startet die Freigabe in der Kontist-App (Push). GiroCrab zeigt „Bitte in der Kontist-App bestätigen“. |
| 4 | `GET /api/user/mfa/challenges/{id}`, dann `POST …/{id}/token` | Abfrage alle 3 Sekunden bis `VERIFIED` (bestätigter Access- und Refresh-Token), `DENIED` oder Ablauf (`expiresAt`). |
| 5 | `POST /api/graphql` `viewer { firstName lastName mainAccount { iban name balance … } }` | Ein Konto `kontist:<IBAN>`, Kontoart Girokonto, EUR, Saldo in Cent. |
| 6 | `POST /api/graphql` `mainAccount.transactions(first: 50, after, filter: { bookingDate_gte })` | Relay-Seiten zu je 50 Buchungen bis `hasNextPage = false`. |

## Regeln

* Passwort und Tokens gehen nur an `https://api.kontist.com`; Weiterleitungen auf andere Hosts werden nicht verfolgt.
* Der bestätigte Refresh-Token liegt im verschlüsselten Verbindungszustand des Tresors. Solange Kontist ihn gelten lässt, laufen weitere Abrufe ohne Passwort und ohne Freigabe; danach fragt GiroCrab wieder nach dem Passwort und der Freigabe.
* Buchungs-ID: `kontist:<id>` aus der API. `amount` ist vorzeichenbehaftet in Cent (Belastungen negativ), `valutaDate` fehlt bei manchen Buchungen und wird dann durch `bookingDate` ersetzt. Der Buchungstext kommt aus `type` (z. B. `SEPA_DIRECT_DEBIT` → „Lastschrift“), unbekannte Typen bleiben im Original.
* Grenzen der API: höchstens 100 Anfragen pro Minute, 50 Buchungen pro Seite.

## Offene Punkte

* Mit einem echten Konto getestet (November 2026): Ein auf kontist.dev angelegter Kunden-Client ohne Secret meldet sich mit `grant_type=password` an; Saldo und Umsätze kommen an.
* Unterkonten (Steuer-Rücklage o. Ä.) werden nicht gelesen, nur `mainAccount`.

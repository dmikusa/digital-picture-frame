# Google Drive Setup

The frame can read a folder from your own Google Drive with a **service
account**. A service account is a robot identity that belongs to a Google Cloud
project you control; you share a normal Drive folder with its email address and
the frame reads that folder with a JSON key you download once.

This is the private-folder setup. It does not ask for your Google password, and
it does not require the folder to be public.

## Why not OAuth or device flow?

- A **web OAuth callback** needs an HTTPS redirect URI. The admin UI runs over
  plain HTTP on a LAN address (`http://192.168.x.x:81xx`), and Google rejects
  non-localhost HTTP redirect URIs. HTTPS for the admin UI is future work.
- The **device flow** is restricted to the `drive.appdata` and `drive.file`
  scopes. Neither can read an existing folder full of photos, so it cannot work
  for this use case.
- A **service account** can read a folder shared with it using the
  `drive.readonly` scope, which is exactly what the frame needs.

## 1. Create or select a Google Cloud project

1. Open <https://console.cloud.google.com> with the same Google account that
   owns the photos.
2. Select an existing project or create one (for example `photo-frame`). Any
   project in that account works; you do not need a separate account or inbox.

## 2. Enable the Google Drive API

1. Go to **APIs & Services → Library**.
2. Search for **Google Drive API** and click **Enable**.

## 3. Create a service account and download a JSON key

1. Go to **APIs & Services → Credentials**.
2. Click **Create credentials → Service account**.
3. Give it a name such as `photo-frame` and finish the wizard. No roles or
   project permissions are needed: access comes from sharing the Drive folder.
4. Open the new service account, go to the **Keys** tab, and choose
   **Add key → Create new key → JSON**.
5. The JSON file downloads once. It contains `client_email`, `private_key`, and
   `token_uri`.

The `client_email` looks like
`photo-frame@your-project.iam.gserviceaccount.com`. That address is the identity
you share the folder with.

## 4. Create a folder and share it

1. In [Google Drive](https://drive.google.com), create a normal folder and put
   the photos in it (subfolders are not scanned).
2. Right-click the folder → **Share**.
3. Paste the service account's `client_email` and give it **Viewer** access.
   Uncheck "Notify people"; a service account cannot receive email.
4. Copy the folder ID from the URL:
   `https://drive.google.com/drive/folders/`**`1AbCdEf...`**

> **The service account cannot own files.** Modern service accounts have no
> storage quota of their own, so do not try to create the folder as the service
> account (for example by impersonating it). Create the folder with your normal
> Google account, then share it. Reading a folder you own is fine.

## 5. Add the source in the admin UI

1. Start config mode (insert a USB drive or use the QR code on screen).
2. Open the admin URL and go to **Add → Google Drive (Private, Service
   Account)**.
3. Fill in:
   - **Name**: any label, for example `Family Drive`.
   - **Folder ID**: the ID from step 4.
   - **Check Interval**: how often to look for new photos, in seconds.
   - **Service Account JSON Key**: paste the entire JSON file.
4. Click **Save**, then **Finish Setup**.

The frame validates the folder when the source first connects. If it reports a
permission error, confirm the folder is shared with the exact `client_email`
from the JSON file as Viewer, and that the Drive API is enabled in the project.

## Security notes

- The JSON key is a password-equivalent secret. Anyone with it can read every
  folder shared with that service account.
- `config.toml` is written with mode `0600` and owned by the service user. Do
  not copy the key into world-readable files or paste it into logs or issues.
- If the key leaks, delete it in **Credentials → Service accounts → Keys**,
  create a new key, and update the source. Deleting the key immediately revokes
  access.
- Grant Viewer access only. The frame never writes to Drive.
- Delete the source in the admin UI (or remove it from `config.toml`) to stop
  syncing, and unshare the folder if you no longer want the frame to read it.

## Public folders (API key)

A folder shared as "Anyone with the link can view" can use an API key instead
of a service account: enable the Drive API, create an API key restricted to it,
and use **Add → Google Drive (Public)**. This exposes the folder to anyone with
the link, so the service-account flow above is preferable for family photos.

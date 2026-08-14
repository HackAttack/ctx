import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.Signature;
import java.security.cert.CertificateFactory;
import java.security.cert.X509Certificate;
import java.time.Instant;
import java.time.format.DateTimeFormatter;
import java.util.Arrays;
import java.util.Collection;
import java.util.Date;
import java.util.List;
import javax.naming.ldap.LdapName;
import javax.naming.ldap.Rdn;
import javax.security.auth.x500.X500Principal;

import net.jsign.DigestAlgorithm;
import net.jsign.Signable;
import net.jsign.bouncycastle.asn1.ASN1Encodable;
import net.jsign.bouncycastle.asn1.ASN1GeneralizedTime;
import net.jsign.bouncycastle.asn1.ASN1ObjectIdentifier;
import net.jsign.bouncycastle.asn1.ASN1OctetString;
import net.jsign.bouncycastle.asn1.ASN1Primitive;
import net.jsign.bouncycastle.asn1.ASN1Sequence;
import net.jsign.bouncycastle.asn1.ASN1UTCTime;
import net.jsign.bouncycastle.asn1.cms.Attribute;
import net.jsign.bouncycastle.asn1.cms.AttributeTable;
import net.jsign.bouncycastle.asn1.cms.CMSAttributes;
import net.jsign.bouncycastle.asn1.cms.ContentInfo;
import net.jsign.bouncycastle.asn1.cms.SignedData;
import net.jsign.bouncycastle.asn1.x509.AlgorithmIdentifier;
import net.jsign.bouncycastle.cert.X509CertificateHolder;
import net.jsign.bouncycastle.cms.CMSSignedData;
import net.jsign.bouncycastle.cms.SignerInformation;
import net.jsign.bouncycastle.tsp.TimeStampToken;

public final class WindowsAuthenticodeInspect {
    private static final String AUTHENTICODE_CONTENT_OID = "1.3.6.1.4.1.311.2.1.4";
    private static final String CODE_SIGNING_EKU = "1.3.6.1.5.5.7.3.3";
    private static final String TIMESTAMPING_EKU = "1.3.6.1.5.5.7.3.8";
    private static final ASN1ObjectIdentifier RFC3161_AUTHENTICODE_TIMESTAMP =
        new ASN1ObjectIdentifier("1.3.6.1.4.1.311.3.3.1");

    private static final class PeCertificateTable {
        private final long offset;
        private final long size;

        private PeCertificateTable(long offset, long size) {
            this.offset = offset;
            this.size = size;
        }

        private long offset() {
            return offset;
        }

        private long size() {
            return size;
        }
    }

    private static void fail(String message) {
        throw new IllegalArgumentException(message);
    }

    private static String json(String value) {
        StringBuilder result = new StringBuilder("\"");
        for (int index = 0; index < value.length(); index++) {
            char item = value.charAt(index);
            switch (item) {
                case '\\':
                    result.append("\\\\");
                    break;
                case '"':
                    result.append("\\\"");
                    break;
                case '\b':
                    result.append("\\b");
                    break;
                case '\f':
                    result.append("\\f");
                    break;
                case '\n':
                    result.append("\\n");
                    break;
                case '\r':
                    result.append("\\r");
                    break;
                case '\t':
                    result.append("\\t");
                    break;
                default:
                    if (item < 0x20) {
                        result.append(String.format("\\u%04x", (int) item));
                    } else {
                        result.append(item);
                    }
                    break;
            }
        }
        return result.append('"').toString();
    }

    private static String hex(byte[] value) {
        char[] encoded = new char[value.length * 2];
        char[] digits = "0123456789abcdef".toCharArray();
        for (int index = 0; index < value.length; index++) {
            int item = Byte.toUnsignedInt(value[index]);
            encoded[index * 2] = digits[item >>> 4];
            encoded[index * 2 + 1] = digits[item & 0x0f];
        }
        return new String(encoded);
    }

    private static String sha256(byte[] value) throws Exception {
        return hex(MessageDigest.getInstance("SHA-256").digest(value));
    }

    private static byte[] derValue(byte[] encoded) {
        if (encoded.length < 2) {
            fail("DER value is truncated");
        }
        int lengthByte = Byte.toUnsignedInt(encoded[1]);
        int header = 2;
        if ((lengthByte & 0x80) != 0) {
            int count = lengthByte & 0x7f;
            if (count == 0 || count > 4 || encoded.length < 2 + count) {
                fail("DER length is unsupported");
            }
            header += count;
        }
        return Arrays.copyOfRange(encoded, header, encoded.length);
    }

    private static String certificateSha256(X509Certificate certificate) throws Exception {
        return sha256(certificate.getEncoded());
    }

    private static String rdn(X500Principal principal, String name) throws Exception {
        LdapName parsed = new LdapName(principal.getName(X500Principal.RFC2253));
        for (Rdn item : parsed.getRdns()) {
            if (item.getType().equalsIgnoreCase(name)) {
                return String.valueOf(item.getValue());
            }
        }
        return "";
    }

    private static void requireEku(X509Certificate certificate, String expected, String label)
        throws Exception {
        List<String> usages = certificate.getExtendedKeyUsage();
        if (usages == null || !usages.contains(expected)) {
            fail(label + " lacks the required extended key usage");
        }
    }

    @SuppressWarnings({"unchecked", "rawtypes"})
    private static X509Certificate certificateFor(
        CMSSignedData signedData,
        SignerInformation signer,
        String label
    ) throws Exception {
        Collection<X509CertificateHolder> matches = (Collection) signedData
            .getCertificates().getMatches(signer.getSID());
        if (matches.size() != 1) {
            fail(label + " has an ambiguous signer certificate");
        }
        X509CertificateHolder holder = matches.iterator().next();
        byte[] encoded = holder.toASN1Structure().getEncoded();
        return (X509Certificate) CertificateFactory.getInstance("X.509")
            .generateCertificate(new ByteArrayInputStream(encoded));
    }

    private static SignerInformation onlySigner(CMSSignedData signedData, String label) {
        Collection<SignerInformation> signers = signedData.getSignerInfos().getSigners();
        if (signers.size() != 1) {
            fail(label + " must contain exactly one signer");
        }
        return signers.iterator().next();
    }

    private static void verifySigner(
        SignerInformation signer,
        X509Certificate certificate,
        byte[] signedContent
    ) throws Exception {
        DigestAlgorithm digestAlgorithm = DigestAlgorithm.of(
            signer.getDigestAlgorithmID().getAlgorithm()
        );
        if (digestAlgorithm != DigestAlgorithm.SHA256) {
            fail("CMS signer digest must be SHA-256");
        }
        AttributeTable attributes = signer.getSignedAttributes();
        Attribute messageDigest = attributes == null
            ? null
            : attributes.get(CMSAttributes.messageDigest);
        if (messageDigest == null || messageDigest.getAttrValues().size() != 1) {
            fail("CMS signer has no unambiguous signed content digest");
        }
        byte[] expectedDigest = ASN1OctetString.getInstance(
            messageDigest.getAttrValues().getObjectAt(0)
        ).getOctets();
        byte[] actualDigest = digestAlgorithm.getMessageDigest().digest(signedContent);
        if (!MessageDigest.isEqual(expectedDigest, actualDigest)) {
            fail("CMS signed content digest verification failed");
        }
        String encryption = signer.getEncryptionAlgOID();
        String algorithm;
        if (encryption.equals("1.2.840.113549.1.1.1")
            || encryption.equals("1.2.840.113549.1.1.11")) {
            algorithm = "SHA256withRSA";
        } else if (encryption.equals("1.2.840.10045.4.3.2")) {
            algorithm = "SHA256withECDSA";
        } else {
            fail("CMS signer uses an unsupported signature algorithm");
            return;
        }
        Signature verifier = Signature.getInstance(algorithm);
        verifier.initVerify(certificate.getPublicKey());
        verifier.update(signer.getEncodedSignedAttributes());
        if (!verifier.verify(signer.getSignature())) {
            fail("CMS signature verification failed");
        }
    }

    private static Date signingTime(SignerInformation signer, String label) throws Exception {
        AttributeTable attributes = signer.getSignedAttributes();
        Attribute value = attributes == null ? null : attributes.get(CMSAttributes.signingTime);
        if (value == null || value.getAttrValues().size() != 1) {
            fail(label + " has no unambiguous signed signing time");
        }
        ASN1Primitive primitive = value.getAttrValues().getObjectAt(0).toASN1Primitive();
        if (primitive instanceof ASN1UTCTime) {
            return ((ASN1UTCTime) primitive).getAdjustedDate();
        }
        if (primitive instanceof ASN1GeneralizedTime) {
            return ((ASN1GeneralizedTime) primitive).getDate();
        }
        fail(label + " uses an unsupported signing-time encoding");
        return null;
    }

    private static PeCertificateTable peCertificateTable(Path artifact) throws Exception {
        byte[] raw = Files.readAllBytes(artifact);
        if (raw.length < 0x40 || raw[0] != 'M' || raw[1] != 'Z') {
            fail("artifact is not a DOS/PE executable");
        }
        ByteBuffer little = ByteBuffer.wrap(raw).order(ByteOrder.LITTLE_ENDIAN);
        long pe = Integer.toUnsignedLong(little.getInt(0x3c));
        if (pe + 24 > raw.length || raw[(int) pe] != 'P' || raw[(int) pe + 1] != 'E'
            || raw[(int) pe + 2] != 0 || raw[(int) pe + 3] != 0) {
            fail("artifact has an invalid PE header");
        }
        int optional = Math.toIntExact(pe + 24);
        if (optional + 152 > raw.length || Short.toUnsignedInt(little.getShort(optional)) != 0x20b) {
            fail("artifact is not a complete PE32+ executable");
        }
        int directory = optional + 112 + 4 * 8;
        long offset = Integer.toUnsignedLong(little.getInt(directory));
        long size = Integer.toUnsignedLong(little.getInt(directory + 4));
        if (offset == 0 || size < 8 || offset % 8 != 0 || offset + size > raw.length) {
            fail("artifact has no valid PE certificate table");
        }
        return new PeCertificateTable(offset, size);
    }

    private static byte[] signedContent(CMSSignedData signedData) throws Exception {
        SignedData structure = SignedData.getInstance(
            signedData.toASN1Structure().getContent()
        );
        ASN1Encodable content = structure.getEncapContentInfo().getContent();
        if (content == null) {
            fail("CMS signature has no embedded signed content");
        }
        return content.toASN1Primitive().getEncoded();
    }

    private static byte[] processableContent(CMSSignedData signedData) throws Exception {
        if (signedData.getSignedContent() == null) {
            return signedContent(signedData);
        }
        ByteArrayOutputStream encoded = new ByteArrayOutputStream();
        signedData.getSignedContent().write(encoded);
        return encoded.toByteArray();
    }

    private static DigestAlgorithm verifyEmbeddedDigest(
        Signable artifact,
        CMSSignedData signedData,
        byte[] content
    ) throws Exception {
        if (!AUTHENTICODE_CONTENT_OID.equals(signedData.getSignedContentTypeOID())) {
            fail("signature does not contain Authenticode indirect data");
        }
        ASN1Sequence indirect = ASN1Sequence.getInstance(
            ASN1Primitive.fromByteArray(content)
        );
        if (indirect.size() != 2) {
            fail("Authenticode indirect data has an unsupported shape");
        }
        ASN1Sequence digestInfo = ASN1Sequence.getInstance(indirect.getObjectAt(1));
        if (digestInfo.size() != 2) {
            fail("Authenticode digest info has an unsupported shape");
        }
        AlgorithmIdentifier identifier = AlgorithmIdentifier.getInstance(
            digestInfo.getObjectAt(0)
        );
        DigestAlgorithm algorithm = DigestAlgorithm.of(identifier.getAlgorithm());
        if (algorithm != DigestAlgorithm.SHA256) {
            fail("Authenticode digest must be SHA-256");
        }
        byte[] embedded = ASN1OctetString.getInstance(digestInfo.getObjectAt(1)).getOctets();
        if (!MessageDigest.isEqual(embedded, artifact.computeDigest(algorithm))) {
            fail("Authenticode digest does not bind the exact PE bytes");
        }
        return algorithm;
    }

    public static void main(String[] args) {
        try {
            if (args.length != 10) {
                fail("usage: WindowsAuthenticodeInspect ARTIFACT OUTPUT AUTHORITY ACCOUNT PROFILE ENDPOINT EXPECTED_CN EXPECTED_O JSIGN_SHA256 TIMESTAMP_URL");
            }
            Path artifactPath = Path.of(args[0]).toAbsolutePath().normalize();
            Path outputPath = Path.of(args[1]).toAbsolutePath().normalize();
            if (!Files.isRegularFile(artifactPath, LinkOption.NOFOLLOW_LINKS)
                || Files.isSymbolicLink(artifactPath)) {
                fail("artifact must be a regular non-symlink file");
            }
            if (Files.exists(outputPath, LinkOption.NOFOLLOW_LINKS)) {
                fail("Authenticode evidence output already exists");
            }
            PeCertificateTable table = peCertificateTable(artifactPath);
            CMSSignedData signature;
            DigestAlgorithm digestAlgorithm;
            X509Certificate signerCertificate;
            SignerInformation signer;
            byte[] authenticodeContent;
            try (Signable signable = Signable.of(artifactPath.toFile())) {
                List<CMSSignedData> signatures = signable.getSignatures();
                if (signatures.size() != 1) {
                    fail("artifact must contain exactly one Authenticode signature");
                }
                signature = signatures.get(0);
                authenticodeContent = signedContent(signature);
                digestAlgorithm = verifyEmbeddedDigest(
                    signable,
                    signature,
                    authenticodeContent
                );
                signer = onlySigner(signature, "Authenticode signature");
                signerCertificate = certificateFor(signature, signer, "Authenticode signature");
                verifySigner(signer, signerCertificate, derValue(authenticodeContent));
            }

            if (!rdn(signerCertificate.getSubjectX500Principal(), "CN").equals(args[6])
                || !rdn(signerCertificate.getSubjectX500Principal(), "O").equals(args[7])) {
                fail("Authenticode signer identity does not match release policy");
            }
            requireEku(signerCertificate, CODE_SIGNING_EKU, "Authenticode signer");
            if (!signerCertificate.getIssuerX500Principal().getName().contains("Microsoft")) {
                fail("Authenticode signer was not issued by Microsoft");
            }

            AttributeTable unsigned = signer.getUnsignedAttributes();
            Attribute timestampAttribute = unsigned == null
                ? null
                : unsigned.get(RFC3161_AUTHENTICODE_TIMESTAMP);
            if (timestampAttribute == null || timestampAttribute.getAttrValues().size() != 1) {
                fail("Authenticode signature has no unambiguous RFC 3161 timestamp");
            }
            ContentInfo timestampContent = ContentInfo.getInstance(
                timestampAttribute.getAttrValues().getObjectAt(0)
            );
            TimeStampToken timestamp = new TimeStampToken(timestampContent);
            CMSSignedData timestampData = timestamp.toCMSSignedData();
            SignerInformation timestampSigner = onlySigner(timestampData, "timestamp token");
            X509Certificate timestampCertificate = certificateFor(
                timestampData,
                timestampSigner,
                "timestamp token"
            );
            verifySigner(
                timestampSigner,
                timestampCertificate,
                processableContent(timestampData)
            );
            requireEku(timestampCertificate, TIMESTAMPING_EKU, "timestamp signer");
            if (!timestampCertificate.getSubjectX500Principal().getName().contains("Microsoft")) {
                fail("timestamp signer identity is not Microsoft");
            }
            DigestAlgorithm imprintAlgorithm = DigestAlgorithm.of(
                timestamp.getTimeStampInfo().getMessageImprintAlgOID()
            );
            if (imprintAlgorithm != DigestAlgorithm.SHA256) {
                fail("timestamp imprint must be SHA-256");
            }
            byte[] imprint = imprintAlgorithm.getMessageDigest().digest(signer.getSignature());
            if (!MessageDigest.isEqual(
                imprint,
                timestamp.getTimeStampInfo().getMessageImprintDigest()
            )) {
                fail("timestamp token does not bind the Authenticode signature");
            }
            Date timestampTime = signingTime(timestampSigner, "timestamp token");
            signerCertificate.checkValidity(timestampTime);
            timestampCertificate.checkValidity(timestampTime);

            byte[] artifactBytes = Files.readAllBytes(artifactPath);
            String evidence = "{\n"
                + "  \"account\": " + json(args[3]) + ",\n"
                + "  \"artifact_sha256\": " + json(sha256(artifactBytes)) + ",\n"
                + "  \"artifact_size\": " + artifactBytes.length + ",\n"
                + "  \"authority\": " + json(args[2]) + ",\n"
                + "  \"certificate_profile\": " + json(args[4]) + ",\n"
                + "  \"certificate_table_offset\": " + table.offset() + ",\n"
                + "  \"certificate_table_size\": " + table.size() + ",\n"
                + "  \"code_signing_endpoint\": " + json(args[5]) + ",\n"
                + "  \"digest_algorithm\": " + json(digestAlgorithm.name()) + ",\n"
                + "  \"jsign_sha256\": " + json(args[8]) + ",\n"
                + "  \"kind\": \"ctx-windows-authenticode-signing\",\n"
                + "  \"schema_version\": 1,\n"
                + "  \"signer_certificate_sha256\": " + json(certificateSha256(signerCertificate)) + ",\n"
                + "  \"signer_issuer\": " + json(signerCertificate.getIssuerX500Principal().getName(X500Principal.RFC2253)) + ",\n"
                + "  \"signer_not_after\": " + json(DateTimeFormatter.ISO_INSTANT.format(signerCertificate.getNotAfter().toInstant())) + ",\n"
                + "  \"signer_not_before\": " + json(DateTimeFormatter.ISO_INSTANT.format(signerCertificate.getNotBefore().toInstant())) + ",\n"
                + "  \"signer_subject\": " + json(signerCertificate.getSubjectX500Principal().getName(X500Principal.RFC2253)) + ",\n"
                + "  \"timestamp_certificate_sha256\": " + json(certificateSha256(timestampCertificate)) + ",\n"
                + "  \"timestamp_signer_subject\": " + json(timestampCertificate.getSubjectX500Principal().getName(X500Principal.RFC2253)) + ",\n"
                + "  \"timestamp_signing_time\": " + json(DateTimeFormatter.ISO_INSTANT.format(timestampTime.toInstant())) + ",\n"
                + "  \"timestamp_url\": " + json(args[9]) + "\n"
                + "}\n";
            Files.createDirectories(outputPath.getParent());
            Files.writeString(outputPath, evidence);
        } catch (Exception error) {
            System.err.println("error: " + error.getMessage());
            System.exit(1);
        }
    }
}
